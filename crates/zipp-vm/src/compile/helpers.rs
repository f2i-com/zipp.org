// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

/// The instruction for an arithmetic/bitwise compound assignment (`dst = a <op>
/// b`). `None` for `=` and the logical-assignment operators (handled separately).
pub(crate) fn compound_assign_instr(op: ox::AssignmentOperator, dst: Reg, a: Reg, b: Reg) -> Option<Instr> {
    use ox::AssignmentOperator as Op;
    Some(match op {
        Op::Addition => Instr::Add { dst, a, b },
        Op::Subtraction => Instr::Sub { dst, a, b },
        Op::Multiplication => Instr::Mul { dst, a, b },
        Op::Division => Instr::Div { dst, a, b },
        Op::Remainder => Instr::Mod { dst, a, b },
        Op::Exponential => Instr::Pow { dst, a, b },
        Op::ShiftLeft => Instr::Bitwise { dst, a, b, op: BitwiseOp::Shl },
        Op::ShiftRight => Instr::Bitwise { dst, a, b, op: BitwiseOp::Shr },
        Op::ShiftRightZeroFill => Instr::Bitwise { dst, a, b, op: BitwiseOp::Ushr },
        Op::BitwiseOR => Instr::Bitwise { dst, a, b, op: BitwiseOp::Or },
        Op::BitwiseXOR => Instr::Bitwise { dst, a, b, op: BitwiseOp::Xor },
        Op::BitwiseAnd => Instr::Bitwise { dst, a, b, op: BitwiseOp::And },
        _ => return None,
    })
}

/// The string of an `export`/`import` ModuleExportName (`foo`, `foo as bar`,
/// `"a-b"`), for recording a module's (exported, local) export pairs.
pub(crate) fn module_export_name(n: &ox::ModuleExportName) -> String {
    match n {
        ox::ModuleExportName::IdentifierName(id) => id.name.to_string(),
        ox::ModuleExportName::IdentifierReference(id) => id.name.to_string(),
        ox::ModuleExportName::StringLiteral(s) => string_literal_key(s),
    }
}

/// A string-literal PROPERTY KEY's text. Property keys are Rust `String`s
/// engine-wide (`ObjMap.keys`), which cannot hold a lone surrogate — a
/// `.lone_surrogates` key decodes LOSSILY (each lone surrogate → U+FFFD, so
/// two distinct lone-surrogate keys collide). Documented stage-2 limit.
pub(crate) fn string_literal_key(s: &ox::StringLiteral) -> String {
    if s.lone_surrogates {
        crate::heap::wtf8_into_lossy_string(crate::heap::decode_lone_surrogate_markers(
            s.value.as_str(),
        ))
    } else {
        s.value.to_string()
    }
}

/// A class member's (non-computed) name. Computed `[expr]` and `#private` names
/// are out of the subset.
pub(crate) fn class_key_name(key: &ox::PropertyKey) -> R<String> {
    match key {
        ox::PropertyKey::StaticIdentifier(id) => Ok(id.name.to_string()),
        ox::PropertyKey::StringLiteral(s) => Ok(string_literal_key(s)),
        ox::PropertyKey::NumericLiteral(n) => Ok(fmt_key_num(n.value)),
        // A BigInt key's property name is its base-10 value string.
        ox::PropertyKey::BigIntLiteral(b) => Ok(b.value.to_string()),
        // A private member `#x`: keyed by "#x" (a reserved property name; the
        // engine does not enforce true hard privacy, but `this.#x` works).
        ox::PropertyKey::PrivateIdentifier(id) => Ok(private_key(&id.name)),
        // A computed well-known-symbol key, e.g. `[Symbol.iterator]() {}`, maps to
        // the reserved string key (so a class can define the iteration method).
        ox::PropertyKey::StaticMemberExpression(m) => {
            if let ox::Expression::Identifier(o) = &m.object {
                if o.name == "Symbol" {
                    // Well-known symbols use the `@@<name>` key convention (matching
                    // the VM's `key_of`), so `[Symbol.toPrimitive]() {}` etc. work.
                    if let Some(k) = well_known_symbol_key(m.property.name.as_str()) {
                        return Ok(k.into());
                    }
                }
            }
            Err("computed or private class member names are not in the zipp-vm subset yet".into())
        }
        _ => Err("computed or private class member names are not in the zipp-vm subset yet".into()),
    }
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
pub(crate) fn hoisted_var_names(body: &[ox::Statement]) -> Vec<String> {
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
pub(crate) fn add_block_lexicals(s: &ox::Statement, out: &mut std::collections::HashSet<String>) {
    use ox::Statement as S;
    match s {
        S::VariableDeclaration(d) if d.kind.is_lexical() => {
            for decl in &d.declarations {
                capture::collect_pattern_names(&decl.id, out);
            }
        }
        S::ClassDeclaration(c) => {
            if let Some(id) = &c.id {
                out.insert(id.name.to_string());
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
    s: &ox::Statement,
    nested: bool,
    blockers: &std::collections::HashSet<String>,
    out: &mut std::collections::HashSet<String>,
) {
    use ox::Statement as S;
    let for_left_lex = |d: &ox::VariableDeclaration, bk: &mut std::collections::HashSet<String>| {
        if d.kind.is_lexical() {
            for decl in &d.declarations {
                capture::collect_pattern_names(&decl.id, bk);
            }
        }
    };
    match s {
        S::FunctionDeclaration(f) => {
            // Annex B B.3.3 applies to PLAIN functions only — generator and
            // async (generator) declarations stay purely block-scoped.
            if nested && !f.generator && !f.r#async {
                if let Some(id) = &f.id {
                    let n = id.name.as_str();
                    if !blockers.contains(n) {
                        out.insert(n.to_string());
                    }
                }
            }
        }
        S::BlockStatement(b) => {
            let mut bk = blockers.clone();
            for st in &b.body {
                add_block_lexicals(st, &mut bk);
            }
            for st in &b.body {
                collect_b33_block_fns(st, true, &bk, out);
            }
        }
        S::ForStatement(f) => {
            let mut bk = blockers.clone();
            if let Some(ox::ForStatementInit::VariableDeclaration(d)) = &f.init {
                for_left_lex(d, &mut bk);
            }
            collect_b33_block_fns(&f.body, true, &bk, out);
        }
        S::ForOfStatement(f) => {
            let mut bk = blockers.clone();
            if let ox::ForStatementLeft::VariableDeclaration(d) = &f.left {
                for_left_lex(d, &mut bk);
            }
            collect_b33_block_fns(&f.body, true, &bk, out);
        }
        S::ForInStatement(f) => {
            let mut bk = blockers.clone();
            if let ox::ForStatementLeft::VariableDeclaration(d) = &f.left {
                for_left_lex(d, &mut bk);
            }
            collect_b33_block_fns(&f.body, true, &bk, out);
        }
        S::WhileStatement(w) => collect_b33_block_fns(&w.body, true, blockers, out),
        S::DoWhileStatement(d) => collect_b33_block_fns(&d.body, true, blockers, out),
        S::IfStatement(i) => {
            collect_b33_block_fns(&i.consequent, true, blockers, out);
            if let Some(a) = &i.alternate {
                collect_b33_block_fns(a, true, blockers, out);
            }
        }
        S::SwitchStatement(sw) => {
            // All cases share one block scope: their lexicals block every case.
            let mut bk = blockers.clone();
            for c in &sw.cases {
                for st in &c.consequent {
                    add_block_lexicals(st, &mut bk);
                }
            }
            for c in &sw.cases {
                for st in &c.consequent {
                    collect_b33_block_fns(st, true, &bk, out);
                }
            }
        }
        S::TryStatement(t) => {
            {
                let mut bk = blockers.clone();
                for st in &t.block.body {
                    add_block_lexicals(st, &mut bk);
                }
                for st in &t.block.body {
                    collect_b33_block_fns(st, true, &bk, out);
                }
            }
            if let Some(h) = &t.handler {
                let mut bk = blockers.clone();
                if let Some(p) = &h.param {
                    // B.3.5: a SIMPLE (BindingIdentifier) catch parameter does NOT
                    // block the B.3.3 var-binding extension — a `var`/block-function
                    // of the same name may redeclare it. A DESTRUCTURING catch param
                    // does block (a matching `var` there is an early error).
                    if !matches!(&p.pattern, ox::BindingPattern::BindingIdentifier(_)) {
                        capture::collect_pattern_names(&p.pattern, &mut bk);
                    }
                }
                for st in &h.body.body {
                    add_block_lexicals(st, &mut bk);
                }
                for st in &h.body.body {
                    collect_b33_block_fns(st, true, &bk, out);
                }
            }
            if let Some(fin) = &t.finalizer {
                let mut bk = blockers.clone();
                for st in &fin.body {
                    add_block_lexicals(st, &mut bk);
                }
                for st in &fin.body {
                    collect_b33_block_fns(st, true, &bk, out);
                }
            }
        }
        S::LabeledStatement(l) => collect_b33_block_fns(&l.body, nested, blockers, out),
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
pub(crate) fn expr_has_yield_or_await(e: &ox::Expression, want_yield: bool, want_await: bool) -> bool {
    use ox::Expression as E;
    macro_rules! r {
        ($x:expr) => {
            expr_has_yield_or_await($x, want_yield, want_await)
        };
    }
    match e {
        E::YieldExpression(y) => {
            want_yield || y.argument.as_ref().map_or(false, |a| r!(a))
        }
        E::AwaitExpression(a) => want_await || r!(&a.argument),
        E::ParenthesizedExpression(p) => r!(&p.expression),
        E::BinaryExpression(b) => r!(&b.left) || r!(&b.right),
        E::LogicalExpression(l) => r!(&l.left) || r!(&l.right),
        E::UnaryExpression(u) => r!(&u.argument),
        E::ConditionalExpression(c) => r!(&c.test) || r!(&c.consequent) || r!(&c.alternate),
        E::SequenceExpression(s) => s.expressions.iter().any(|x| r!(x)),
        E::AssignmentExpression(a) => r!(&a.right),
        E::CallExpression(c) => {
            r!(&c.callee)
                || c.arguments.iter().any(|a| a.as_expression().map_or(false, |x| r!(x)))
        }
        E::NewExpression(n) => {
            r!(&n.callee)
                || n.arguments.iter().any(|a| a.as_expression().map_or(false, |x| r!(x)))
        }
        E::TemplateLiteral(t) => t.expressions.iter().any(|x| r!(x)),
        E::TaggedTemplateExpression(t) => {
            r!(&t.tag) || t.quasi.expressions.iter().any(|x| r!(x))
        }
        E::StaticMemberExpression(m) => r!(&m.object),
        E::ComputedMemberExpression(m) => r!(&m.object) || r!(&m.expression),
        _ => false,
    }
}

/// The [Yield]/[Await] early-error scan over a binding pattern's nested
/// DEFAULT-VALUE expressions and computed keys (the FormalParameter space).
pub(crate) fn pattern_has_yield_or_await(pat: &ox::BindingPattern, want_yield: bool, want_await: bool) -> bool {
    use ox::BindingPattern as P;
    match pat {
        P::BindingIdentifier(_) => false,
        P::AssignmentPattern(ap) => {
            expr_has_yield_or_await(&ap.right, want_yield, want_await)
                || pattern_has_yield_or_await(&ap.left, want_yield, want_await)
        }
        P::ObjectPattern(op) => {
            op.properties.iter().any(|prop| {
                (prop.computed
                    && prop
                        .key
                        .as_expression()
                        .map_or(false, |ke| expr_has_yield_or_await(ke, want_yield, want_await)))
                    || pattern_has_yield_or_await(&prop.value, want_yield, want_await)
            }) || op
                .rest
                .as_ref()
                .map_or(false, |rest| pattern_has_yield_or_await(&rest.argument, want_yield, want_await))
        }
        P::ArrayPattern(arr) => {
            arr.elements
                .iter()
                .flatten()
                .any(|el| pattern_has_yield_or_await(el, want_yield, want_await))
                || arr
                    .rest
                    .as_ref()
                    .map_or(false, |rest| {
                        pattern_has_yield_or_await(&rest.argument, want_yield, want_await)
                    })
        }
    }
}

pub(crate) fn stmt_contains_with(s: &ox::Statement) -> bool {
    use ox::Statement as S;
    match s {
        S::WithStatement(_) => true,
        S::BlockStatement(b) => b.body.iter().any(stmt_contains_with),
        S::IfStatement(i) => {
            stmt_contains_with(&i.consequent)
                || i.alternate.as_ref().is_some_and(stmt_contains_with)
        }
        S::WhileStatement(w) => stmt_contains_with(&w.body),
        S::DoWhileStatement(d) => stmt_contains_with(&d.body),
        S::ForStatement(f) => stmt_contains_with(&f.body),
        S::ForOfStatement(f) => stmt_contains_with(&f.body),
        S::ForInStatement(f) => stmt_contains_with(&f.body),
        S::TryStatement(t) => {
            t.block.body.iter().any(stmt_contains_with)
                || t.handler.as_ref().is_some_and(|h| h.body.body.iter().any(stmt_contains_with))
                || t.finalizer.as_ref().is_some_and(|f| f.body.iter().any(stmt_contains_with))
        }
        S::SwitchStatement(sw) => {
            sw.cases.iter().any(|c| c.consequent.iter().any(stmt_contains_with))
        }
        S::LabeledStatement(l) => stmt_contains_with(&l.body),
        _ => false,
    }
}

pub(crate) fn collect_hoisted_vars(s: &ox::Statement, out: &mut std::collections::HashSet<String>) {
    use ox::Statement as S;
    match s {
        S::VariableDeclaration(d) if d.kind == ox::VariableDeclarationKind::Var => {
            for decl in &d.declarations {
                capture::collect_pattern_names(&decl.id, out);
            }
        }
        // `export var x` / `export var {x} = ...`: the declared names hoist
        // exactly like an unexported top-level var.
        S::ExportNamedDeclaration(e) => {
            if let Some(ox::Declaration::VariableDeclaration(d)) = &e.declaration {
                if d.kind == ox::VariableDeclarationKind::Var {
                    for decl in &d.declarations {
                        capture::collect_pattern_names(&decl.id, out);
                    }
                }
            }
        }
        S::BlockStatement(b) => {
            for s in &b.body {
                collect_hoisted_vars(s, out);
            }
        }
        S::IfStatement(i) => {
            collect_hoisted_vars(&i.consequent, out);
            if let Some(a) = &i.alternate {
                collect_hoisted_vars(a, out);
            }
        }
        S::WhileStatement(w) => collect_hoisted_vars(&w.body, out),
        S::DoWhileStatement(d) => collect_hoisted_vars(&d.body, out),
        S::ForStatement(f) => {
            if let Some(ox::ForStatementInit::VariableDeclaration(d)) = &f.init {
                if d.kind == ox::VariableDeclarationKind::Var {
                    for decl in &d.declarations {
                        capture::collect_pattern_names(&decl.id, out);
                    }
                }
            }
            collect_hoisted_vars(&f.body, out);
        }
        S::ForOfStatement(f) => {
            if let ox::ForStatementLeft::VariableDeclaration(d) = &f.left {
                if d.kind == ox::VariableDeclarationKind::Var {
                    for decl in &d.declarations {
                        capture::collect_pattern_names(&decl.id, out);
                    }
                }
            }
            collect_hoisted_vars(&f.body, out);
        }
        S::ForInStatement(f) => {
            if let ox::ForStatementLeft::VariableDeclaration(d) = &f.left {
                if d.kind == ox::VariableDeclarationKind::Var {
                    for decl in &d.declarations {
                        capture::collect_pattern_names(&decl.id, out);
                    }
                }
            }
            collect_hoisted_vars(&f.body, out);
        }
        S::TryStatement(t) => {
            for s in &t.block.body {
                collect_hoisted_vars(s, out);
            }
            if let Some(h) = &t.handler {
                for s in &h.body.body {
                    collect_hoisted_vars(s, out);
                }
            }
            if let Some(f) = &t.finalizer {
                for s in &f.body {
                    collect_hoisted_vars(s, out);
                }
            }
        }
        S::SwitchStatement(sw) => {
            for case in &sw.cases {
                for s in &case.consequent {
                    collect_hoisted_vars(s, out);
                }
            }
        }
        S::LabeledStatement(l) => collect_hoisted_vars(&l.body, out),
        S::WithStatement(w) => collect_hoisted_vars(&w.body, out),
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
pub(crate) fn for_left_name(left: &ox::ForStatementLeft) -> R<String> {
    match left {
        ox::ForStatementLeft::VariableDeclaration(d) => match &d.declarations[0].id {
            ox::BindingPattern::BindingIdentifier(id) => Ok(id.name.to_string()),
            _ => Err("for-of/for-in destructuring not in the zipp-vm subset yet".into()),
        },
        ox::ForStatementLeft::AssignmentTargetIdentifier(id) => Ok(id.name.to_string()),
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
/// `.lone_surrogates` literal is excluded — its bytes need the WTF-8-decoding
/// constant slot, not a plain `string_constants` entry.
pub(crate) fn concat_key_literal_prefix<'a, 'b>(
    key: &'a ox::Expression<'b>,
) -> Option<(&'a str, &'a ox::Expression<'b>)> {
    if let ox::Expression::BinaryExpression(b) = key {
        if matches!(b.operator, ox::BinaryOperator::Addition) {
            if let ox::Expression::StringLiteral(s) = &b.left {
                if !s.lone_surrogates {
                    return Some((s.value.as_str(), &b.right));
                }
            }
        }
    }
    None
}

pub(crate) fn is_numeric_expr(e: &ox::Expression) -> bool {
    use ox::Expression as E;
    match e {
        E::NumericLiteral(_) => true,
        E::ParenthesizedExpression(p) => is_numeric_expr(&p.expression),
        E::UnaryExpression(u) => matches!(
            u.operator,
            ox::UnaryOperator::UnaryNegation | ox::UnaryOperator::UnaryPlus
        ),
        E::BinaryExpression(b) => matches!(
            b.operator,
            ox::BinaryOperator::Subtraction
                | ox::BinaryOperator::Multiplication
                | ox::BinaryOperator::Division
                | ox::BinaryOperator::Remainder
        ),
        _ => false,
    }
}

/// Extract (fixed param names, optional rest-param name, body statements) from
/// an oxc function.
pub(crate) fn function_parts<'a>(
    f: &'a ox::Function,
) -> R<(Vec<String>, Option<String>, &'a [ox::Statement<'a>])> {
    let params = param_slot_names(&f.params)?;
    let rest = rest_name(&f.params)?;
    let body = match &f.body {
        Some(b) => b.statements.as_slice(),
        None => &[],
    };
    Ok((params, rest, body))
}

/// One name per parameter SLOT (reg 1..). A plain identifier (or `x = default`)
/// uses its name; a destructuring pattern (`{a}` / `[a,b]`) gets a synthetic
/// slot name and is destructured into its leaves at function entry by
/// `bind_pattern_params`.
/// ExpectedArgumentCount → the function's `.length`: the count of leading formal
/// parameters before the first one with a default value (an AssignmentPattern).
/// A destructuring parameter without a default counts; the rest parameter lives
/// in `params.rest`, not `items`, so it is excluded automatically.
/// IsAnonymousFunctionDefinition: an anonymous function/arrow/class expression
/// (function/generator/async-function expressions count when they have no `id`).
/// Such a value takes the property/binding name via NamedEvaluation.
pub(crate) fn is_anonymous_fn_def(e: &ox::Expression) -> bool {
    match e {
        ox::Expression::FunctionExpression(f) => f.id.is_none(),
        ox::Expression::ArrowFunctionExpression(_) => true,
        ox::Expression::ClassExpression(c) => c.id.is_none(),
        ox::Expression::ParenthesizedExpression(p) => is_anonymous_fn_def(&p.expression),
        _ => false,
    }
}

pub(crate) fn expected_arg_count(params: &ox::FormalParameters) -> u16 {
    let mut n = 0u16;
    for item in &params.items {
        // A default value lives in `item.initializer` (simple params) or as an
        // AssignmentPattern (destructuring defaults); the first such param stops
        // the count.
        if item.initializer.is_some()
            || matches!(&item.pattern, ox::BindingPattern::AssignmentPattern(_))
        {
            break;
        }
        n += 1;
    }
    n
}

/// IsSimpleParameterList: every parameter a plain identifier with no default,
/// and no rest. (A mapped arguments object requires this in sloppy mode.)
pub(crate) fn params_are_simple(params: &ox::FormalParameters) -> bool {
    params.rest.is_none()
        && params.items.iter().all(|item| {
            item.initializer.is_none()
                && matches!(&item.pattern, ox::BindingPattern::BindingIdentifier(_))
        })
}

pub(crate) fn param_slot_names(params: &ox::FormalParameters) -> R<Vec<String>> {
    let mut out = Vec::new();
    for (i, item) in params.items.iter().enumerate() {
        match &item.pattern {
            ox::BindingPattern::BindingIdentifier(id) => out.push(id.name.to_string()),
            ox::BindingPattern::AssignmentPattern(ap) => match &ap.left {
                ox::BindingPattern::BindingIdentifier(id) => out.push(id.name.to_string()),
                _ => return Err("a default on a destructuring parameter is not in the subset yet".into()),
            },
            ox::BindingPattern::ObjectPattern(_) | ox::BindingPattern::ArrayPattern(_) => {
                out.push(format!("<arg{i}>"))
            }
        }
    }
    Ok(out)
}

/// All parameter binding identifiers in source order, duplicates preserved
/// (for strict-mode early-error checks: `eval`/`arguments` and duplicate names).
pub(crate) fn collect_param_names_ordered(params: &ox::FormalParameters, out: &mut Vec<String>) {
    pub(crate) fn walk(p: &ox::BindingPattern, out: &mut Vec<String>) {
        use ox::BindingPattern as P;
        match p {
            P::BindingIdentifier(id) => out.push(id.name.to_string()),
            P::AssignmentPattern(ap) => walk(&ap.left, out),
            P::ObjectPattern(op) => {
                for prop in &op.properties {
                    walk(&prop.value, out);
                }
                if let Some(rest) = &op.rest {
                    walk(&rest.argument, out);
                }
            }
            P::ArrayPattern(arr) => {
                for el in arr.elements.iter().flatten() {
                    walk(el, out);
                }
                if let Some(rest) = &arr.rest {
                    walk(&rest.argument, out);
                }
            }
        }
    }
    for item in &params.items {
        walk(&item.pattern, out);
    }
    if let Some(r) = &params.rest {
        walk(&r.rest.argument, out);
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
pub(crate) fn param_pattern_leaves(params: &ox::FormalParameters) -> Vec<String> {
    let mut set = HashSet::new();
    for item in &params.items {
        if matches!(
            &item.pattern,
            ox::BindingPattern::ObjectPattern(_) | ox::BindingPattern::ArrayPattern(_)
        ) {
            capture::collect_pattern_names(&item.pattern, &mut set);
        }
    }
    // A destructuring rest parameter (`...[a,b]`) introduces its leaves too.
    if let Some(r) = &params.rest {
        if matches!(
            &r.rest.argument,
            ox::BindingPattern::ObjectPattern(_) | ox::BindingPattern::ArrayPattern(_)
        ) {
            capture::collect_pattern_names(&r.rest.argument, &mut set);
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
pub(crate) fn rest_name(params: &ox::FormalParameters) -> R<Option<String>> {
    match &params.rest {
        None => Ok(None),
        Some(r) => match &r.rest.argument {
            ox::BindingPattern::BindingIdentifier(id) => Ok(Some(id.name.to_string())),
            ox::BindingPattern::ObjectPattern(_) | ox::BindingPattern::ArrayPattern(_) => {
                Ok(Some("<rest>".to_string()))
            }
            _ => Err("rest-parameter destructuring is not in the zipp-vm subset yet".into()),
        },
    }
}
