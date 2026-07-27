// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

/// The instruction for an arithmetic/bitwise compound assignment (`dst = a <op>
/// b`). `None` for `=` and the logical-assignment operators (handled separately).
pub(crate) fn compound_assign_instr(op: AssignOp, dst: Reg, a: Reg, b: Reg) -> Option<Instr> {
    use crate::parse::ast::AssignOp as Op;
    Some(match op {
        Op::Add => Instr::Add { dst, a, b },
        Op::Sub => Instr::Sub { dst, a, b },
        Op::Mul => Instr::Mul { dst, a, b },
        Op::Div => Instr::Div { dst, a, b },
        Op::Rem => Instr::Mod { dst, a, b },
        Op::Exp => Instr::Pow { dst, a, b },
        Op::Shl => Instr::Bitwise { dst, a, b, op: BitwiseOp::Shl },
        Op::Shr => Instr::Bitwise { dst, a, b, op: BitwiseOp::Shr },
        Op::UShr => Instr::Bitwise { dst, a, b, op: BitwiseOp::Ushr },
        Op::BitOr => Instr::Bitwise { dst, a, b, op: BitwiseOp::Or },
        Op::BitXor => Instr::Bitwise { dst, a, b, op: BitwiseOp::Xor },
        Op::BitAnd => Instr::Bitwise { dst, a, b, op: BitwiseOp::And },
        // `=`, and `&&=`/`||=`/`??=` — the logical forms SHORT-CIRCUIT, so they
        // are not sugar for the arithmetic ones and are compiled separately.
        _ => return None,
    })
}

/// The string of an `export`/`import` ModuleExportName (`foo`, `foo as bar`,
/// `"a-b"`), for recording a module's (exported, local) export pairs.
pub(crate) fn module_export_name(n: &ModuleExportName) -> String {
    match n {
        // oxc split the identifier spelling into IdentifierName vs
        // IdentifierReference by which side of the `as` it sat on; both were
        // read identically here, and the AST does not carry that distinction.
        ModuleExportName::Ident(id) => id.to_string(),
        ModuleExportName::Str(s) => string_literal_key(s),
    }
}

/// A string-literal PROPERTY KEY's text. Property keys are Rust `String`s
/// engine-wide (`ObjMap.keys`), which cannot hold a lone surrogate — a
/// [`StrVal::Utf16`] key decodes LOSSILY (each lone surrogate → U+FFFD, so
/// two distinct lone-surrogate keys collide). Documented stage-2 limit.
pub(crate) fn string_literal_key(s: &StrVal) -> String {
    // Same lossy result as before, one step shorter: a literal that needed
    // oxc's `.lone_surrogates` marker form is a `StrVal::Utf16`, and
    // `to_lossy_string` is `String::from_utf16_lossy` — U+FFFD per lone
    // surrogate, exactly what `wtf8_into_lossy_string(decode_lone_surrogate_
    // markers(..))` produced from the marker text.
    s.to_lossy_string()
}

/// A class member's (non-computed) name. Computed `[expr]` and `#private` names
/// are out of the subset.
pub(crate) fn class_key_name(key: &PropKey) -> R<String> {
    match key {
        PropKey::Ident(id) => Ok(id.to_string()),
        PropKey::Str(s) => Ok(string_literal_key(s)),
        PropKey::Num(n) => Ok(fmt_key_num(*n)),
        // A private member `#x`: keyed by "#x" (a reserved property name; the
        // engine does not enforce true hard privacy, but `this.#x` works).
        PropKey::Private(id) => Ok(private_key(id)),
        // COMPUTED keys. oxc's `PropertyKey` inherited the whole `Expression`
        // enum, so `["a"]`, `[1]`, `[1n]` and `[Symbol.iterator]` arrived at the
        // very same arms as their non-computed spellings — this function never
        // saw the `computed` flag and so never distinguished them. Computed is a
        // VARIANT now, so the same four literal shapes are re-matched through it
        // to keep every answer identical.
        PropKey::Computed(e) => match e {
            Expr::Str(s) => Ok(string_literal_key(s)),
            Expr::Num(n) => Ok(fmt_key_num(*n)),
            // A BigInt key's property name is its base-10 value string.
            Expr::BigInt(b) => Ok(b.to_string()),
            // A computed well-known-symbol key, e.g. `[Symbol.iterator]() {}`, maps to
            // the reserved string key (so a class can define the iteration method).
            Expr::Member(m) => {
                if let (Expr::Ident(o), MemberProp::Ident(p)) = (&m.object, &m.prop) {
                    if &**o == "Symbol" {
                        // Well-known symbols use the `@@<name>` key convention (matching
                        // the VM's `key_of`), so `[Symbol.toPrimitive]() {}` etc. work.
                        if let Some(k) = well_known_symbol_key(p) {
                            return Ok(k.into());
                        }
                    }
                }
                Err("computed or private class member names are not in the zipp-vm subset yet".into())
            }
            _ => Err("computed or private class member names are not in the zipp-vm subset yet".into()),
        },
    }
    // NOTE: a NON-computed BigInt key (`class C { 1n(){} }`) had an arm here and
    // returned the base-10 digits. `PropKey` cannot say it — an f64 cannot
    // round-trip the digits — so the AST rejects it before the compiler runs
    // (`oxc_bridge`: "unsupported: BigInt property key"). Nothing can be done
    // about that from this side; the computed spelling `[1n]` still works.
}

/// The property key for a private member `#name` — keyed by "#name" (the leading
/// `#` makes it un-spellable as a normal property, our soft-privacy stand-in).
pub(crate) fn private_key(name: &str) -> String {
    if name.starts_with('#') {
        name.to_string()
    } else {
        format!("#{name}")
    }
}

/// Recognise the built-in Error constructor names the subset supports. Returns
/// the canonical `name` to store on the error object.
pub(crate) fn error_ctor(name: &str) -> Option<&'static str> {
    Some(match name {
        "Error" => "Error",
        "TypeError" => "TypeError",
        "RangeError" => "RangeError",
        "SyntaxError" => "SyntaxError",
        "ReferenceError" => "ReferenceError",
        "EvalError" => "EvalError",
        "URIError" => "URIError",
        "AggregateError" => "AggregateError",
        _ => return None,
    })
}

/// Collect the names introduced by top-level `var` declarations, recursing
/// through nested statements (blocks, loops, `if`, `try`, `switch`, labels,
/// `with`) but NOT into nested function/class bodies — `var` hoists to the
/// enclosing function/script scope, stopping at a function boundary. `let`/
/// `const`/`class` are excluded (they keep TDZ — a forward read throws). These
/// slots are pre-initialized to `undefined` so var hoisting matches JS.
/// All `var` binding names declared anywhere in `body` (recursing through blocks/
/// loops/if/try/switch but not nested functions). These bind in FUNCTION scope, so a
/// nested closure over one must be in the capture set to box the right register.
pub(crate) fn hoisted_var_names(body: &[Stmt]) -> Vec<String> {
    let mut set = std::collections::HashSet::new();
    for s in body {
        collect_hoisted_vars(s, &mut set);
    }
    sorted_name_vec(&set)
}

/// A name set as a sorted `Vec`.
///
/// `std::collections::HashSet` reseeds its hasher every process, so iterating
/// one to assign global slots or registers made the COMPILER nondeterministic:
/// the same source produced different bytecode on every run (measured: 5 runs,
/// 5 distinct dumps, 474 of 889 lines differing), and
/// `Object.getOwnPropertyNames(globalThis)` permuted run to run. Any iteration
/// whose order reaches a slot number, a register, or emitted code goes through
/// here.
///
/// Sorted, not source-ordered: source order would have to be threaded through
/// `collect_pattern_names` and its 36 call sites. Sorting is deterministic,
/// which is what the bytecode-differential gate needs. (zipp's global property
/// order already differs from V8's for an unrelated reason — hoisted function
/// declarations take slots before top-level vars — so this does not trade away
/// a conformance property it currently has.)
pub(crate) fn sorted_name_vec(set: &std::collections::HashSet<String>) -> Vec<String> {
    let mut v: Vec<String> = set.iter().cloned().collect();
    v.sort_unstable();
    v
}


/// Add a block's DIRECT lexical declaration names (top-level `let`/`const`/
/// `class` of the block) to `out` — the names that block Annex B B.3.3 for a
/// same-block function declaration.
pub(crate) fn add_block_lexicals(s: &Stmt, out: &mut std::collections::HashSet<String>) {
    use crate::parse::ast::Stmt as S;
    match s {
        S::VarDecl(d) if d.kind.is_lexical() => {
            for decl in &d.decls {
                capture::collect_pattern_names(&decl.id, out);
            }
        }
        S::ClassDecl(c) => {
            if let Some(id) = &c.name {
                out.insert(id.to_string());
            }
        }
        _ => {}
    }
}

/// Annex B B.3.3: collect names of `function` declarations inside BLOCKS (not at
/// the top level of the function body) that are eligible for a function-scoped
/// `var` binding. `blockers` is the set of lexical names in scope (params,
/// top-level lexicals, plus the lexical declarations of every enclosing block /
/// for-head / catch param): a function whose name is blocked would be an early
/// error under B.3.3 and so is SKIPPED (left block-local).
pub(crate) fn collect_b33_block_fns(
    s: &Stmt,
    nested: bool,
    blockers: &std::collections::HashSet<String>,
    out: &mut std::collections::HashSet<String>,
) {
    use crate::parse::ast::Stmt as S;
    let for_left_lex = |d: &VarDecl, bk: &mut std::collections::HashSet<String>| {
        if d.kind.is_lexical() {
            for decl in &d.decls {
                capture::collect_pattern_names(&decl.id, bk);
            }
        }
    };
    match s {
        S::FnDecl(f) => {
            // Annex B B.3.3 applies to PLAIN functions only — generator and
            // async (generator) declarations stay purely block-scoped.
            if nested && !f.is_generator && !f.is_async {
                if let Some(id) = &f.name {
                    let n: &str = id;
                    if !blockers.contains(n) {
                        out.insert(n.to_string());
                    }
                }
            }
        }
        S::Block(body) => {
            let mut bk = blockers.clone();
            for st in body {
                add_block_lexicals(st, &mut bk);
            }
            for st in body {
                collect_b33_block_fns(st, true, &bk, out);
            }
        }
        S::For { init, body, .. } => {
            let mut bk = blockers.clone();
            if let Some(ForInit::Var(d)) = init {
                for_left_lex(d, &mut bk);
            }
            collect_b33_block_fns(body, true, &bk, out);
        }
        S::ForOf { left, body, .. } => {
            let mut bk = blockers.clone();
            if let ForTarget::Var(d) = left {
                for_left_lex(d, &mut bk);
            }
            collect_b33_block_fns(body, true, &bk, out);
        }
        S::ForIn { left, body, .. } => {
            let mut bk = blockers.clone();
            if let ForTarget::Var(d) = left {
                for_left_lex(d, &mut bk);
            }
            collect_b33_block_fns(body, true, &bk, out);
        }
        S::While { body, .. } => collect_b33_block_fns(body, true, blockers, out),
        S::DoWhile { body, .. } => collect_b33_block_fns(body, true, blockers, out),
        S::If { cons, alt, .. } => {
            collect_b33_block_fns(cons, true, blockers, out);
            if let Some(a) = alt {
                collect_b33_block_fns(a, true, blockers, out);
            }
        }
        S::Switch { cases, .. } => {
            // All cases share one block scope: their lexicals block every case.
            let mut bk = blockers.clone();
            for c in cases {
                for st in &c.body {
                    add_block_lexicals(st, &mut bk);
                }
            }
            for c in cases {
                for st in &c.body {
                    collect_b33_block_fns(st, true, &bk, out);
                }
            }
        }
        S::Try { block, handler, finalizer } => {
            {
                let mut bk = blockers.clone();
                for st in block {
                    add_block_lexicals(st, &mut bk);
                }
                for st in block {
                    collect_b33_block_fns(st, true, &bk, out);
                }
            }
            if let Some(h) = handler {
                let mut bk = blockers.clone();
                if let Some(p) = &h.param {
                    // B.3.5: a SIMPLE (BindingIdentifier) catch parameter does NOT
                    // block the B.3.3 var-binding extension — a `var`/block-function
                    // of the same name may redeclare it. A DESTRUCTURING catch param
                    // does block (a matching `var` there is an early error).
                    if !matches!(p, Pattern::Ident(_)) {
                        capture::collect_pattern_names(p, &mut bk);
                    }
                }
                for st in &h.body {
                    add_block_lexicals(st, &mut bk);
                }
                for st in &h.body {
                    collect_b33_block_fns(st, true, &bk, out);
                }
            }
            if let Some(fin) = finalizer {
                let mut bk = blockers.clone();
                for st in fin {
                    add_block_lexicals(st, &mut bk);
                }
                for st in fin {
                    collect_b33_block_fns(st, true, &bk, out);
                }
            }
        }
        S::Labeled { body, .. } => collect_b33_block_fns(body, nested, blockers, out),
        _ => {}
    }
}

/// Whether a statement (transitively, NOT descending into nested function
/// bodies) contains a `with` statement — gates the pre-declare-all-vars pass.
/// Early-error scan: does this expression contain a YieldExpression
/// (`want_yield`) / AwaitExpression (`want_await`) in ITS OWN grammar context?
/// Used on generator/async FormalParameter initializers, where both are
/// SyntaxErrors (also covering the dynamic `GeneratorFunction('x = yield','')`
/// forms). A nested function/arrow/class body opens a fresh [Yield]/[Await]
/// context, so unknown/function-like nodes conservatively contribute false.
pub(crate) fn expr_has_yield_or_await(e: &Expr, want_yield: bool, want_await: bool) -> bool {
    use crate::parse::ast::Expr as E;
    macro_rules! r {
        ($x:expr) => {
            expr_has_yield_or_await($x, want_yield, want_await)
        };
    }
    match e {
        E::Yield { arg, .. } => {
            want_yield || arg.as_ref().map_or(false, |a| r!(a))
        }
        E::Await(a) => want_await || r!(a),
        E::Binary { left, right, .. } => r!(left) || r!(right),
        E::Logical { left, right, .. } => r!(left) || r!(right),
        E::Unary { arg, .. } => r!(arg),
        E::Cond { test, cons, alt } => r!(test) || r!(cons) || r!(alt),
        E::Seq(xs) => xs.iter().any(|x| r!(x)),
        E::Assign { value, .. } => r!(value),
        E::Call(c) => {
            r!(&c.callee)
                || c.args.iter().any(|a| match a {
                    Arg::Expr(x) => r!(x),
                    // A SPREAD argument contributed `false`: oxc's
                    // `Argument::as_expression()` returns `None` for one, and
                    // the scan took that as "nothing to see".
                    Arg::Spread(_) => false,
                })
        }
        E::New { callee, args } => {
            r!(callee)
                || args.iter().any(|a| match a {
                    Arg::Expr(x) => r!(x),
                    Arg::Spread(_) => false,
                })
        }
        E::Template(t) => t.exprs.iter().any(|x| r!(x)),
        E::TaggedTemplate { tag, quasi } => {
            r!(tag) || quasi.exprs.iter().any(|x| r!(x))
        }
        E::Member(m) => match &m.prop {
            // StaticMemberExpression: the object only — the property is a name.
            MemberProp::Ident(_) => r!(&m.object),
            // ComputedMemberExpression: object and key.
            MemberProp::Computed(k) => r!(&m.object) || r!(k),
            // NOTE: `a.#b` (oxc's PrivateFieldExpression) had NO arm and so
            // contributed `false`, object included. The three member forms are
            // one node now, so that omission has to be spelled out to keep the
            // same answer. (Widening it would reject programs accepted today.)
            MemberProp::Private(_) => false,
        },
        // NOTE: an optional chain (`Expr::Chain`) still contributes `false`,
        // because oxc's `ChainExpression` had no arm either — `f(x = a?.b(yield))`
        // is accepted today. Kept as-is: this scan decides SyntaxErrors, and
        // widening it would reject programs that currently compile.
        _ => false,
    }
}

/// 15.5.1 / 15.8.1: a generator's or async function's FormalParameters are
/// parsed with [~Yield]/[~Await], so a `yield`/`await` anywhere in them — a
/// default value or a computed key — is an early SyntaxError. Both body
/// compilers (`compile_function_body` and `compile_class_fn`) must run this;
/// class methods used to skip it, which let `class { *g(x = yield) {} }`
/// through while the object-literal form was rejected.
pub(crate) fn check_params_yield_await(
    params_ast: Option<&crate::parse::ast::Params>,
    is_generator: bool,
    is_async: bool,
) -> Result<(), String> {
    if !(is_generator || is_async) {
        return Ok(());
    }
    if let Some(pa) = params_ast {
        if pa
            .items
            .iter()
            .any(|item| pattern_has_yield_or_await(item, is_generator, is_async))
        {
            return Err(
                "SyntaxError: yield/await expression not permitted in formal parameters".into(),
            );
        }
    }
    Ok(())
}

/// The [Yield]/[Await] early-error scan over a binding pattern's nested
/// DEFAULT-VALUE expressions and computed keys (the FormalParameter space).
pub(crate) fn pattern_has_yield_or_await(pat: &Pattern, want_yield: bool, want_await: bool) -> bool {
    use crate::parse::ast::Pattern as P;
    match pat {
        P::Ident(_) => false,
        P::Assign { left, right } => {
            expr_has_yield_or_await(right, want_yield, want_await)
                || pattern_has_yield_or_await(left, want_yield, want_await)
        }
        P::Object { props, rest } => {
            props.iter().any(|prop| {
                matches!(&prop.key, PropKey::Computed(ke)
                    if expr_has_yield_or_await(ke, want_yield, want_await))
                    || pattern_has_yield_or_await(&prop.value, want_yield, want_await)
            }) || rest
                .as_ref()
                .map_or(false, |rest| pattern_has_yield_or_await(rest, want_yield, want_await))
        }
        P::Array(elems) => elems
            .iter()
            .flatten()
            .any(|el| pattern_has_yield_or_await(&el.pat, want_yield, want_await)),
        // The rest element lives IN the element list now (last), rather than in
        // a sibling field, so the walk above reaches it through this arm — same
        // set of nodes visited as when `arr.rest` was scanned separately.
        P::Rest(arg) => pattern_has_yield_or_await(arg, want_yield, want_await),
    }
}

pub(crate) fn stmt_contains_with(s: &Stmt) -> bool {
    use crate::parse::ast::Stmt as S;
    match s {
        S::With { .. } => true,
        S::Block(b) => b.iter().any(stmt_contains_with),
        S::If { cons, alt, .. } => {
            stmt_contains_with(cons) || alt.as_ref().is_some_and(|a| stmt_contains_with(a))
        }
        S::While { body, .. } => stmt_contains_with(body),
        S::DoWhile { body, .. } => stmt_contains_with(body),
        S::For { body, .. } => stmt_contains_with(body),
        S::ForOf { body, .. } => stmt_contains_with(body),
        S::ForIn { body, .. } => stmt_contains_with(body),
        S::Try { block, handler, finalizer } => {
            block.iter().any(stmt_contains_with)
                || handler.as_ref().is_some_and(|h| h.body.iter().any(stmt_contains_with))
                || finalizer.as_ref().is_some_and(|f| f.iter().any(stmt_contains_with))
        }
        S::Switch { cases, .. } => cases.iter().any(|c| c.body.iter().any(stmt_contains_with)),
        S::Labeled { body, .. } => stmt_contains_with(body),
        _ => false,
    }
}

pub(crate) fn collect_hoisted_vars(s: &Stmt, out: &mut std::collections::HashSet<String>) {
    use crate::parse::ast::Stmt as S;
    match s {
        S::VarDecl(d) if d.kind == VarKind::Var => {
            for decl in &d.decls {
                capture::collect_pattern_names(&decl.id, out);
            }
        }
        // `export var x` / `export var {x} = ...`: the declared names hoist
        // exactly like an unexported top-level var.
        S::Export(e) => {
            if let ExportDecl::Decl(d) = &**e {
                if let S::VarDecl(d) = &**d {
                    if d.kind == VarKind::Var {
                        for decl in &d.decls {
                            capture::collect_pattern_names(&decl.id, out);
                        }
                    }
                }
            }
        }
        S::Block(b) => {
            for s in b {
                collect_hoisted_vars(s, out);
            }
        }
        S::If { cons, alt, .. } => {
            collect_hoisted_vars(cons, out);
            if let Some(a) = alt {
                collect_hoisted_vars(a, out);
            }
        }
        S::While { body, .. } => collect_hoisted_vars(body, out),
        S::DoWhile { body, .. } => collect_hoisted_vars(body, out),
        S::For { init, body, .. } => {
            if let Some(ForInit::Var(d)) = init {
                if d.kind == VarKind::Var {
                    for decl in &d.decls {
                        capture::collect_pattern_names(&decl.id, out);
                    }
                }
            }
            collect_hoisted_vars(body, out);
        }
        S::ForOf { left, body, .. } => {
            if let ForTarget::Var(d) = left {
                if d.kind == VarKind::Var {
                    for decl in &d.decls {
                        capture::collect_pattern_names(&decl.id, out);
                    }
                }
            }
            collect_hoisted_vars(body, out);
        }
        S::ForIn { left, body, .. } => {
            if let ForTarget::Var(d) = left {
                if d.kind == VarKind::Var {
                    for decl in &d.decls {
                        capture::collect_pattern_names(&decl.id, out);
                    }
                }
            }
            collect_hoisted_vars(body, out);
        }
        S::Try { block, handler, finalizer } => {
            for s in block {
                collect_hoisted_vars(s, out);
            }
            if let Some(h) = handler {
                for s in &h.body {
                    collect_hoisted_vars(s, out);
                }
            }
            if let Some(f) = finalizer {
                for s in f {
                    collect_hoisted_vars(s, out);
                }
            }
        }
        S::Switch { cases, .. } => {
            for case in cases {
                for s in &case.body {
                    collect_hoisted_vars(s, out);
                }
            }
        }
        S::Labeled { body, .. } => collect_hoisted_vars(body, out),
        S::With { body, .. } => collect_hoisted_vars(body, out),
        _ => {}
    }
}

/// The internal property key for a well-known symbol (`Symbol.<name>`), matching
/// the VM's `WELL_KNOWN_SYMBOLS` / `key_of` convention. `None` for non-well-known
/// names (a computed `[Symbol.foo]` that isn't a known symbol stays unsupported).
pub(crate) fn well_known_symbol_key(name: &str) -> Option<&'static str> {
    Some(match name {
        "iterator" => "@@iterator",
        "asyncIterator" => "@@asyncIterator",
        "toPrimitive" => "@@toPrimitive",
        "toStringTag" => "@@toStringTag",
        "hasInstance" => "@@hasInstance",
        "isConcatSpreadable" => "@@isConcatSpreadable",
        "species" => "@@species",
        "match" => "@@match",
        "matchAll" => "@@matchAll",
        "replace" => "@@replace",
        "search" => "@@search",
        "split" => "@@split",
        "unscopables" => "@@unscopables",
        "dispose" => "@@dispose",
        "asyncDispose" => "@@asyncDispose",
        _ => return None,
    })
}

/// The canonical index of an error constructor name (parallel to the VM's
/// `ERROR_NAMES` / `error_protos`). Unknown → 0 (`Error`).
pub(crate) fn error_kind_index(name: &str) -> u8 {
    match name {
        "TypeError" => 1,
        "RangeError" => 2,
        "SyntaxError" => 3,
        "ReferenceError" => 4,
        "EvalError" => 5,
        "URIError" => 6,
        "AggregateError" => 7,
        _ => 0, // "Error" and anything unexpected
    }
}

/// Extract the loop-variable name from a `for-of`/`for-in` left-hand side.
/// Supports `for (let/const/var x of …)` and `for (x of …)`.
pub(crate) fn for_left_name(left: &ForTarget) -> R<String> {
    match left {
        // A for-in/of head has exactly one declarator by the grammar, which is
        // what made indexing safe here before and still does.
        ForTarget::Var(d) => match &d.decls[0].id {
            Pattern::Ident(id) => Ok(id.to_string()),
            _ => Err("for-of/for-in destructuring not in the zipp-vm subset yet".into()),
        },
        // `covered` (a parenthesised `(x)`) is deliberately ignored: it changes
        // NamedEvaluation and simple-target early errors, neither of which a
        // for-in/of head can observe, so reading it would change behaviour.
        ForTarget::Target(Target::Ident { name, .. }) => Ok(name.to_string()),
        _ => Err("for-of/for-in needs a simple variable target".into()),
    }
}

/// Render a numeric object key the way JS does (`{0: 'a'}` has key `"0"`).
pub(crate) fn fmt_key_num(n: f64) -> String {
    // The property key for a numeric literal is ToString(value) — the canonical
    // ECMAScript Number→String (e.g. 0.0000001 → "1e-7", 1e21 → "1e+21"), the SAME
    // form the runtime uses for `obj[n]`, so a numeric-keyed member is stored and
    // read under one key. (Rust's `{}` differs for small/large magnitudes.)
    crate::vm::helpers_num2::fmt_f64(n)
}

/// Conservative static check: is this expression definitely a number? Used to
/// gate the `+ <int>` fast path (where `+` could otherwise mean string concat).
/// Only returns true for cases that cannot be strings.
/// If `key` is `<plain string literal> + <rhs>`, return `(prefix text, rhs)`.
/// Recognises the `obj["prefix" + i]` computed-member-key idiom so the read/write
/// can fuse the throwaway concat key (see `Instr::GetIndexConcat`). A
/// lone-surrogate literal is excluded — its bytes need the WTF-8-decoding
/// constant slot, not a plain `string_constants` entry.
pub(crate) fn concat_key_literal_prefix(key: &Expr) -> Option<(&str, &Expr)> {
    if let Expr::Binary { op: BinaryOp::Add, left, right } = key {
        // `StrVal::Utf8` IS the not-`.lone_surrogates` case: a literal holding a
        // lone surrogate can only be `StrVal::Utf16`, and `StrVal::from_utf16`
        // collapses back to `Utf8` exactly when the text is well-formed.
        if let Expr::Str(StrVal::Utf8(s)) = &**left {
            return Some((s.as_str(), right));
        }
    }
    None
}

pub(crate) fn is_numeric_expr(e: &Expr) -> bool {
    use crate::parse::ast::Expr as E;
    match e {
        E::Num(_) => true,
        E::Unary { op, .. } => matches!(op, UnaryOp::Minus | UnaryOp::Plus),
        E::Binary { op, .. } => matches!(
            op,
            BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Rem
        ),
        _ => false,
    }
}

/// Extract (fixed param names, optional rest-param name, body statements) from
/// a function node.
pub(crate) fn function_parts(f: &Function) -> R<(Vec<String>, Option<String>, &[Stmt])> {
    let params = param_slot_names(&f.params)?;
    let rest = rest_name(&f.params)?;
    // `FnBody` is not optional: a bodiless function is a TypeScript declaration
    // or overload signature, which the AST cannot represent at all, so the
    // "no body → no statements" case is gone rather than silently empty.
    let body = f.body.stmts.as_slice();
    Ok((params, rest, body))
}

/// One name per parameter SLOT (reg 1..). A plain identifier (or `x = default`)
/// uses its name; a destructuring pattern (`{a}` / `[a,b]`) gets a synthetic
/// slot name and is destructured into its leaves at function entry by
/// `bind_pattern_params`.
/// ExpectedArgumentCount → the function's `.length`: the count of leading formal
/// parameters before the first one with a default value (an AssignmentPattern).
/// A destructuring parameter without a default counts; the rest parameter lives
/// last in `params.items`, not in a sibling field, so it is excluded explicitly.
/// IsAnonymousFunctionDefinition: an anonymous function/arrow/class expression
/// (function/generator/async-function expressions count when they have no `id`).
/// Such a value takes the property/binding name via NamedEvaluation.
pub(crate) fn is_anonymous_fn_def(e: &Expr) -> bool {
    match e {
        Expr::Function(f) => f.name.is_none(),
        Expr::Arrow(_) => true,
        Expr::Class(c) => c.name.is_none(),
        _ => false,
    }
}

pub(crate) fn expected_arg_count(params: &Params) -> u16 {
    let mut n = 0u16;
    for item in &params.items {
        match item {
            // A default value. oxc parked it in `FormalParameter::initializer`
            // beside the pattern; the AST wraps the pattern in `Pattern::Assign`
            // instead, and either way the FIRST such parameter stops the count.
            Pattern::Assign { .. } => break,
            // The rest parameter is not a formal parameter for `.length`, and it
            // is always last — so reaching it also ends the count. (It used to
            // live outside `items` and was excluded automatically.)
            Pattern::Rest(_) => break,
            _ => n += 1,
        }
    }
    n
}

/// IsSimpleParameterList: every parameter a plain identifier with no default,
/// and no rest. (A mapped arguments object requires this in sloppy mode.)
pub(crate) fn params_are_simple(params: &Params) -> bool {
    // Precomputed by the front end (`Params::simple`, `items.iter().all(Ident)`),
    // which is the same predicate this used to evaluate: a rest element and a
    // defaulted parameter are both non-`Ident` items. Reading it rather than
    // recomputing keeps the two from ever disagreeing.
    params.simple
}

pub(crate) fn param_slot_names(params: &Params) -> R<Vec<String>> {
    let mut out = Vec::new();
    for (i, item) in params.items.iter().enumerate() {
        match item {
            Pattern::Ident(id) => out.push(id.to_string()),
            // `x = 1` and `{a} = {}`. oxc kept the default in a sibling
            // `initializer` field, so this saw the BARE pattern and treated the
            // two exactly as it treats the undefaulted spellings below; the
            // wrapper is peeled here to keep that true.
            Pattern::Assign { left, .. } => match &**left {
                Pattern::Ident(id) => out.push(id.to_string()),
                Pattern::Object { .. } | Pattern::Array(_) => out.push(format!("<arg{i}>")),
                _ => return Err("a default on a destructuring parameter is not in the subset yet".into()),
            },
            Pattern::Object { .. } | Pattern::Array(_) => out.push(format!("<arg{i}>")),
            // The rest parameter is the last item now instead of a sibling
            // field; it gets no fixed slot (`rest_name` owns it), and nothing
            // can follow it.
            Pattern::Rest(_) => break,
        }
    }
    Ok(out)
}

/// All parameter binding identifiers in source order, duplicates preserved
/// (for strict-mode early-error checks: `eval`/`arguments` and duplicate names).
pub(crate) fn collect_param_names_ordered(params: &Params, out: &mut Vec<String>) {
    pub(crate) fn walk(p: &Pattern, out: &mut Vec<String>) {
        use crate::parse::ast::Pattern as P;
        match p {
            P::Ident(id) => out.push(id.to_string()),
            P::Assign { left, .. } => walk(left, out),
            P::Object { props, rest } => {
                for prop in props {
                    walk(&prop.value, out);
                }
                if let Some(rest) = rest {
                    walk(rest, out);
                }
            }
            P::Array(elems) => {
                for el in elems.iter().flatten() {
                    walk(&el.pat, out);
                }
            }
            P::Rest(arg) => walk(arg, out),
        }
    }
    // The rest parameter is the last `items` entry (a `Pattern::Rest`), so one
    // loop produces the same order the items-then-rest pair of loops did.
    for item in &params.items {
        walk(item, out);
    }
}

/// Strict-mode early error: `eval` and `arguments` may not be used as a binding
/// name or assignment target in strict-mode code. Returns a `SyntaxError`-prefixed
/// error (mapped to a thrown SyntaxError by the eval/compile entry points).
pub(crate) fn strict_name_err(strict: bool, name: &str) -> R<()> {
    if !strict {
        return Ok(());
    }
    if name == "eval" || name == "arguments" {
        return Err(format!(
            "SyntaxError: '{name}' may not be used as a binding name or assignment target in strict mode"
        ));
    }
    if is_strict_reserved_word(name) {
        return Err(format!(
            "SyntaxError: '{name}' is a reserved word in strict mode"
        ));
    }
    Ok(())
}

/// The ECMAScript identifiers reserved ONLY in strict mode — they may not be used
/// as a binding name, assignment target, or identifier reference there. The six
/// FutureReservedWords plus the contextual keywords `let`/`static`/`yield` (these
/// reach a binding/reference position as plain identifiers only where they are NOT
/// the declaration keyword / class modifier / YieldExpression). All are valid
/// identifiers in sloppy mode.
pub(crate) fn is_strict_reserved_word(name: &str) -> bool {
    matches!(
        name,
        "implements"
            | "interface"
            | "package"
            | "private"
            | "protected"
            | "public"
            | "let"
            | "static"
            | "yield"
    )
}

/// Leaf binding names introduced by destructuring parameters (for capture
/// analysis — a closure may capture a destructured parameter's leaf).
pub(crate) fn param_pattern_leaves(params: &Params) -> Vec<String> {
    let mut set = HashSet::new();
    for item in &params.items {
        // A defaulted parameter wraps its pattern now (`{a} = {}` is
        // `Pattern::Assign`), where oxc kept `item.pattern` as the bare
        // ObjectPattern and hung the default off `initializer`. Peel it so the
        // same destructuring parameters are recognised.
        let pat = match item {
            Pattern::Assign { left, .. } => &**left,
            other => other,
        };
        match pat {
            Pattern::Object { .. } | Pattern::Array(_) => {
                capture::collect_pattern_names(pat, &mut set)
            }
            // A destructuring rest parameter (`...[a,b]`) introduces its leaves too.
            Pattern::Rest(arg) => {
                if matches!(&**arg, Pattern::Object { .. } | Pattern::Array(_)) {
                    capture::collect_pattern_names(arg, &mut set);
                }
            }
            _ => {}
        }
    }
    set.into_iter().collect()
}

/// All bindable parameter names (fixed params plus the rest name, if any) — the
/// set capture analysis must consider locals of this function.
pub(crate) fn with_rest(params: &[String], rest: &Option<String>) -> Vec<String> {
    let mut v = params.to_vec();
    if let Some(r) = rest {
        v.push(r.clone());
    }
    v
}

/// The rest-parameter SLOT name (`function f(...args)` → `Some("args")`), or
/// `None`. A destructuring rest target (`...[a,b]` / `...{x}`) uses a synthetic
/// slot `"<rest>"` that holds the gathered array; `bind_params` then destructures
/// it into the pattern's leaves.
pub(crate) fn rest_name(params: &Params) -> R<Option<String>> {
    // The rest element is the LAST item rather than a sibling field — the
    // grammar rejects anything after it, so only the last item can be one.
    match params.items.last() {
        Some(Pattern::Rest(arg)) => match &**arg {
            Pattern::Ident(id) => Ok(Some(id.to_string())),
            Pattern::Object { .. } | Pattern::Array(_) => Ok(Some("<rest>".to_string())),
            _ => Err("rest-parameter destructuring is not in the zipp-vm subset yet".into()),
        },
        _ => Ok(None),
    }
}
