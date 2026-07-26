//! Lowering from the oxc AST to zipp's own.
//!
//! Temporary scaffolding with a precise purpose: it lets the compiler be
//! retargeted onto the new AST as a change that is KNOWN to be a no-op, gated on
//! `zipp bcdiff` staying green, before any hand-written parser exists to add
//! risk. It is deleted when the parser lands.
//!
//! Reaching the end state means changing two independent things — the front end,
//! and the compiler's input type across 12.5k lines. Doing both at once would
//! make the first bytecode mismatch ambiguous between a parser bug, an AST
//! design flaw, and a retarget slip. With this in between, each step has exactly
//! one suspect: retarget the compiler onto this (the parser is provably not
//! involved), then swap the parser underneath it (the compiler is provably not
//! involved).
//!
//! One thing it cannot exercise: oxc has no way to represent a call as an
//! assignment target, so [`Target::Call`] is unreachable through here and only
//! becomes live with the real parser.

use oxc_ast::ast as ox;

use super::ast::*;
use super::token::{Span, StrVal};

type R<T> = Result<T, String>;

// Whether the code currently being lowered is strict. The parser threads this
// through `Ctx`; these free functions have no context parameter and adding one
// means touching every signature in a file that exists to be deleted — so a
// thread-local carries it instead. Scaffolding-grade on purpose: the bridge dies
// when the parser is wired in, and the real parser does this properly.
thread_local! {
    static STRICT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run `f` with the strictness cell set to `on`, restoring the previous value —
/// including on the error path, or one failed lowering would poison the rest.
fn with_strict<T>(on: bool, f: impl FnOnce() -> R<T>) -> R<T> {
    let prev = STRICT.with(|c| c.replace(on));
    let out = f();
    STRICT.with(|c| c.set(prev));
    out
}

/// oxc span -> ours.
fn sp(s: oxc_span::Span) -> Span {
    Span::new(s.start, s.end)
}

/// An oxc identifier/atom -> our `Name`.
fn name(s: &str) -> Name {
    s.into()
}

/// A cooked oxc string -> [`StrVal`].
///
/// oxc never hands back raw UTF-16. When a literal contains a lone surrogate it
/// sets `lone_surrogates` and encodes `value` with U+FFFD as an ESCAPE
/// character: each lone surrogate becomes U+FFFD plus four lowercase hex digits,
/// and a literal U+FFFD becomes U+FFFD followed by `fffd`. Honouring that flag
/// here is the whole reason [`StrVal`] exists — without it the surrogate is
/// silently replaced. Unflagged text passes through verbatim, because an
/// ordinary string may legitimately contain U+FFFD followed by hex digits.
fn str_val(value: &str, lone_surrogates: bool) -> StrVal {
    if !lone_surrogates {
        return StrVal::Utf8(value.to_string());
    }
    let mut units: Vec<u16> = Vec::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c == '\u{FFFD}' {
            let rest = chars.as_str();
            if rest.len() >= 4 && rest.as_bytes()[..4].iter().all(u8::is_ascii_hexdigit) {
                if let Ok(unit) = u32::from_str_radix(&rest[..4], 16) {
                    for _ in 0..4 {
                        chars.next();
                    }
                    units.push(unit as u16);
                    continue;
                }
            }
        }
        let mut buf = [0u16; 2];
        units.extend_from_slice(c.encode_utf16(&mut buf));
    }
    StrVal::from_utf16(units)
}

/// The same, for an oxc `StringLiteral` node.
fn str_lit(s: &ox::StringLiteral) -> StrVal {
    str_val(&s.value, s.lone_surrogates)
}

// NOTE (1): `str_val` below is the ONE place oxc's lone-surrogate marker
// encoding is decoded. Other sections need it too (property keys, directives,
// module specifiers, import attributes), so it is written here and must appear
// exactly ONCE in the assembled file — do not redefine it in another section.
// oxc never exposes raw UTF-16 or a `Vec<u16>`: `StringLiteral`/`TemplateElement`
// carry a `lone_surrogates: bool` and, when it is set, encode `value`/`cooked`
// with U+FFFD as an escape character (`\u{FFFD}XXXX` per lone code unit,
// `\u{FFFD}fffd` for a literal U+FFFD). That is why this is not just
// `StrVal::Utf8`. `crate::heap::decode_lone_surrogate_markers` does the same
// decode to WTF-8 bytes; this one goes straight to UTF-16 units for `StrVal`.
//
// NOTE (2): CoverInitializedName — `ObjectMember::Prop { init }` is ALWAYS
// `None` through this bridge. oxc does not put the initializer in the tree at
// all: `parse_property_definition` stashes the whole `a = 1` in parser state
// (`ParserState::cover_initialized_name`, keyed by span) and the `ObjectProperty`
// keeps `value` = the plain identifier. That side map is consumed when the
// literal is reinterpreted as a destructuring target (where it becomes
// `AssignmentTargetPropertyIdentifier::init`, i.e. `TargetProp::default`, owned
// by the targets section) and is otherwise reported as a SyntaxError at end of
// parse. So nothing is dropped here — there is nothing to carry. The field only
// becomes reachable when the hand-written parser lands.
//
// NOTE (3): `Target::Ident { covered }` is ALWAYS `false` through this bridge.
// oxc's `AssignmentTarget`/`SimpleAssignmentTarget` have no parenthesized
// variant, and `SimpleAssignmentTarget::cover` (oxc_parser `js/grammar.rs`)
// unwraps and discards `ParenthesizedExpression` when it converts the cover
// grammar. So `(x) = function(){}` is indistinguishable from `x = function(){}`
// once oxc has parsed it. This is a fidelity loss the bridge cannot avoid, but
// not a regression: the current oxc-based compiler is blind to it in exactly the
// same way, so `bcdiff` stays green.
//
// NOTE (4): `#brand in obj` was a genuine representability gap the AST had —
// `compile/exprs.rs:494` handles `PrivateInExpression` today, so a brand check
// would have failed the differential gate. `Expr::PrivateIn` was added for it
// rather than bending `MemberProp::Private`, which is the wrong shape (there is
// no object being keyed).
//
// NOTE (5): `Target::Call` (Annex B `f() = 1`) is unreachable through the
// bridge — oxc's `AssignmentTarget` cannot represent a call, which is the very
// limitation the new AST exists to remove. Only the hand-written parser can
// produce it, so `annexb_call_target_rewrite` stays load-bearing until then.
//
// NOTE (6): `Expr::Regex.pattern` is oxc's raw source slice, so a lone surrogate
// written literally (not escaped) in an eval'd WTF-8 source reads as U+FFFD
// here. `compile/mod.rs`'s `exact_src` recovers those bytes by span at compile
// time; the bridge has no source buffer and cannot. Flags are re-spelled from
// oxc's `RegExpFlags` bitset in canonical order — identical to the
// `to_inline_string()` the compiler already uses, so no bytecode difference.
//
// NOTE (7): helper names defined by THIS section, for de-duplication when the
// sections are concatenated: `str_val`, `lower_expr`, `lower_opt_expr`,
// `lower_static_member`, `lower_computed_member`, `lower_private_member`,
// `lower_member`, `lower_call`, `lower_chain_element`, `lower_args`,
// `lower_simple_target`, `lower_array_elems`, `lower_object`,
// `lower_accessor_fn`, `lower_template`, `lower_template_element`, `unary_op`,
// `update_op`, `binary_op`, `logical_op`, `assign_op`.

// ============================================================================
// Expressions
// ============================================================================



fn lower_expr(e: &ox::Expression) -> R<Expr> {
    Ok(match e {
        // ---- literals ------------------------------------------------------
        ox::Expression::BooleanLiteral(b) => Expr::Bool(b.value),
        ox::Expression::NullLiteral(_) => Expr::Null,
        ox::Expression::NumericLiteral(n) => Expr::Num(n.value),
        // oxc has already normalised the spelling to base 10 without
        // separators, which is exactly what `BigInt` parsing wants.
        ox::Expression::BigIntLiteral(b) => Expr::BigInt(b.value.as_str().into()),
        ox::Expression::StringLiteral(s) => Expr::Str(str_val(&s.value, s.lone_surrogates)),
        ox::Expression::RegExpLiteral(r) => Expr::Regex {
            // The pattern is the raw source between the slashes — the regex
            // engine compiles the source, so escapes stay escaped.
            pattern: StrVal::Utf8(r.regex.pattern.text.to_string()),
            // `RegExpFlags` is a bitset, so this re-spells the flags in the
            // spec's canonical order. Not observable: `RegExp.prototype.flags`
            // (and hence `toString`) builds that same order regardless of how
            // the literal was written, and a duplicate flag is a SyntaxError.
            flags: r.regex.flags.to_string().into_boxed_str(),
        },
        ox::Expression::TemplateLiteral(t) => Expr::Template(Box::new(lower_template(t)?)),

        // ---- primaries -----------------------------------------------------
        ox::Expression::Identifier(i) => Expr::Ident(name(&i.name)),
        ox::Expression::ThisExpression(_) => Expr::This,
        ox::Expression::Super(_) => Expr::Super,
        // oxc funnels both meta properties through one node; ours splits them,
        // and nothing else is a legal MetaProperty.
        ox::Expression::MetaProperty(m) => match (m.meta.name.as_str(), m.property.name.as_str()) {
            ("import", "meta") => Expr::ImportMeta,
            ("new", "target") => Expr::NewTarget,
            (meta, prop) => return Err(format!("unsupported: meta property `{meta}.{prop}`")),
        },

        // ---- object / array literals ---------------------------------------
        ox::Expression::ArrayExpression(a) => Expr::Array(lower_array_elems(&a.elements)?),
        ox::Expression::ObjectExpression(o) => Expr::Object(lower_object(o)?),

        // ---- functions and classes -----------------------------------------
        ox::Expression::ArrowFunctionExpression(a) => Expr::Arrow(Box::new(lower_arrow(a)?)),
        ox::Expression::FunctionExpression(f) => Expr::Function(Box::new(lower_function(f)?)),
        ox::Expression::ClassExpression(c) => Expr::Class(Box::new(lower_class(c)?)),

        // ---- operators -----------------------------------------------------
        ox::Expression::UnaryExpression(u) => {
            Expr::Unary { op: unary_op(u.operator), arg: Box::new(lower_expr(&u.argument)?) }
        }
        // Ours takes a `Target`, not an `Expr`, because `++` writes back.
        ox::Expression::UpdateExpression(u) => Expr::Update {
            op: update_op(u.operator),
            prefix: u.prefix,
            target: lower_simple_target(&u.argument)?,
        },
        ox::Expression::BinaryExpression(b) => Expr::Binary {
            op: binary_op(b.operator),
            left: Box::new(lower_expr(&b.left)?),
            right: Box::new(lower_expr(&b.right)?),
        },
        ox::Expression::LogicalExpression(l) => Expr::Logical {
            op: logical_op(l.operator),
            left: Box::new(lower_expr(&l.left)?),
            right: Box::new(lower_expr(&l.right)?),
        },
        ox::Expression::AssignmentExpression(a) => Expr::Assign {
            op: assign_op(a.operator),
            target: lower_target(&a.left)?,
            value: Box::new(lower_expr(&a.right)?),
        },
        ox::Expression::ConditionalExpression(c) => Expr::Cond {
            test: Box::new(lower_expr(&c.test)?),
            cons: Box::new(lower_expr(&c.consequent)?),
            alt: Box::new(lower_expr(&c.alternate)?),
        },
        ox::Expression::SequenceExpression(s) => {
            let mut out = Vec::with_capacity(s.expressions.len());
            for e in &s.expressions {
                out.push(lower_expr(e)?);
            }
            Expr::Seq(out)
        }

        // ---- calls, members, chains ----------------------------------------
        ox::Expression::CallExpression(c) => Expr::Call(Box::new(lower_call(c)?)),
        ox::Expression::NewExpression(n) => Expr::New {
            callee: Box::new(lower_expr(&n.callee)?),
            args: lower_args(&n.arguments)?,
        },
        ox::Expression::ComputedMemberExpression(m) => {
            Expr::Member(Box::new(lower_computed_member(m)?))
        }
        ox::Expression::StaticMemberExpression(m) => Expr::Member(Box::new(lower_static_member(m)?)),
        ox::Expression::PrivateFieldExpression(m) => {
            Expr::Member(Box::new(lower_private_member(m)?))
        }
        // Both ASTs scope short-circuiting the same way — one wrapper around the
        // whole chain, `optional` on the links — so this is a direct wrap. The
        // wrapper is what makes `(a?.b).c` differ from `a?.b.c`: parenthesising
        // ends the chain, and oxc's node boundary already says so.
        ox::Expression::ChainExpression(c) => {
            Expr::Chain(Box::new(lower_chain_element(&c.expression)?))
        }

        // ---- the rest ------------------------------------------------------
        ox::Expression::AwaitExpression(a) => Expr::Await(Box::new(lower_expr(&a.argument)?)),
        ox::Expression::YieldExpression(y) => {
            Expr::Yield { arg: lower_opt_expr(&y.argument)?.map(Box::new), delegate: y.delegate }
        }
        ox::Expression::TaggedTemplateExpression(t) => Expr::TaggedTemplate {
            tag: Box::new(lower_expr(&t.tag)?),
            quasi: Box::new(lower_template(&t.quasi)?),
        },
        ox::Expression::ImportExpression(i) => Expr::ImportCall {
            spec: Box::new(lower_expr(&i.source)?),
            options: lower_opt_expr(&i.options)?.map(Box::new),
            phase: match i.phase {
                None => ImportPhase::Evaluation,
                Some(ox::ImportPhase::Source) => ImportPhase::Source,
                Some(ox::ImportPhase::Defer) => ImportPhase::Defer,
            },
        },
        // Parenthesisation is not a node for us. It is observable only on an
        // assignment target, and oxc has already dropped it there (its
        // `AssignmentTarget` has no parenthesized form — see NOTE 3), so
        // unwrapping here loses nothing the bridge could have recovered anyway.
        ox::Expression::ParenthesizedExpression(p) => lower_expr(&p.expression)?,

        // `#brand in obj` — a brand check, not a member access: the private name
        // is the LEFT operand of `in`, with no object being keyed.
        ox::Expression::PrivateInExpression(e) => Expr::PrivateIn {
            name: name(e.left.name.as_str()),
            object: Box::new(lower_expr(&e.right)?),
        },
        ox::Expression::JSXElement(_) | ox::Expression::JSXFragment(_) => {
            return Err("unsupported: JSX".to_string());
        }
        ox::Expression::V8IntrinsicExpression(_) => {
            return Err("unsupported: V8 intrinsic (`%Foo()`)".to_string());
        }
        ox::Expression::TSAsExpression(_)
        | ox::Expression::TSSatisfiesExpression(_)
        | ox::Expression::TSTypeAssertion(_)
        | ox::Expression::TSNonNullExpression(_)
        | ox::Expression::TSInstantiationExpression(_) => {
            return Err("unsupported: TypeScript expression".to_string());
        }
    })
}

fn lower_opt_expr(e: &Option<ox::Expression>) -> R<Option<Expr>> {
    match e {
        Some(e) => Ok(Some(lower_expr(e)?)),
        None => Ok(None),
    }
}

// ----------------------------------------------------------------------------
// Members, calls, chains
// ----------------------------------------------------------------------------

fn lower_static_member(m: &ox::StaticMemberExpression) -> R<Member> {
    Ok(Member {
        object: lower_expr(&m.object)?,
        prop: MemberProp::Ident(name(&m.property.name)),
        optional: m.optional,
    })
}

fn lower_computed_member(m: &ox::ComputedMemberExpression) -> R<Member> {
    Ok(Member {
        object: lower_expr(&m.object)?,
        prop: MemberProp::Computed(lower_expr(&m.expression)?),
        optional: m.optional,
    })
}

fn lower_private_member(m: &ox::PrivateFieldExpression) -> R<Member> {
    Ok(Member {
        object: lower_expr(&m.object)?,
        // A private name is not a string key and must not become one: `a.#b`
        // resolves against the class's private environment, not `a`'s
        // properties.
        prop: MemberProp::Private(name(&m.field.name)),
        optional: m.optional,
    })
}

fn lower_member(m: &ox::MemberExpression) -> R<Member> {
    match m {
        ox::MemberExpression::ComputedMemberExpression(m) => lower_computed_member(m),
        ox::MemberExpression::StaticMemberExpression(m) => lower_static_member(m),
        ox::MemberExpression::PrivateFieldExpression(m) => lower_private_member(m),
    }
}

fn lower_call(c: &ox::CallExpression) -> R<CallExpr> {
    Ok(CallExpr {
        callee: lower_expr(&c.callee)?,
        args: lower_args(&c.arguments)?,
        // `a?.()`. Set on the CALL, not the chain, so `a?.b()` (which calls
        // unconditionally once `a?.b` has not short-circuited) stays distinct.
        optional: c.optional,
    })
}

/// One link of an optional chain. The links are ordinary member/call nodes in
/// both ASTs; only the `Chain` wrapper is special.
fn lower_chain_element(c: &ox::ChainElement) -> R<Expr> {
    match c {
        ox::ChainElement::CallExpression(call) => Ok(Expr::Call(Box::new(lower_call(call)?))),
        ox::ChainElement::TSNonNullExpression(_) => {
            Err("unsupported: TypeScript `!` inside an optional chain".to_string())
        }
        other => {
            let m = other
                .as_member_expression()
                .ok_or_else(|| "unsupported: optional-chain element".to_string())?;
            Ok(Expr::Member(Box::new(lower_member(m)?)))
        }
    }
}

fn lower_args(args: &[ox::Argument]) -> R<Vec<Arg>> {
    let mut out = Vec::with_capacity(args.len());
    for a in args {
        out.push(match a {
            ox::Argument::SpreadElement(s) => Arg::Spread(lower_expr(&s.argument)?),
            other => {
                let e =
                    other.as_expression().ok_or_else(|| "unsupported: call argument".to_string())?;
                Arg::Expr(lower_expr(e)?)
            }
        });
    }
    Ok(out)
}



// ----------------------------------------------------------------------------
// Array and object literals
// ----------------------------------------------------------------------------

fn lower_array_elems(els: &[ox::ArrayExpressionElement]) -> R<Vec<Option<ArrayElem>>> {
    let mut out = Vec::with_capacity(els.len());
    for el in els {
        out.push(match el {
            // A HOLE, which is NOT `undefined`: `0 in [, 1]` is false, and
            // `Array.prototype.forEach` skips it.
            ox::ArrayExpressionElement::Elision(_) => None,
            ox::ArrayExpressionElement::SpreadElement(s) => {
                Some(ArrayElem::Spread(lower_expr(&s.argument)?))
            }
            other => {
                let e =
                    other.as_expression().ok_or_else(|| "unsupported: array element".to_string())?;
                Some(ArrayElem::Expr(lower_expr(e)?))
            }
        });
    }
    Ok(out)
}

fn lower_object(o: &ox::ObjectExpression) -> R<Vec<ObjectMember>> {
    let mut out = Vec::with_capacity(o.properties.len());
    for p in &o.properties {
        match p {
            ox::ObjectPropertyKind::SpreadProperty(s) => {
                out.push(ObjectMember::Spread(lower_expr(&s.argument)?));
            }
            ox::ObjectPropertyKind::ObjectProperty(p) => {
                let key = lower_prop_key(&p.key, p.computed)?;
                // SPAN. A method's [[SourceText]] is the whole
                // MethodDefinition — `next () { … }`, `[Symbol.iterator] () { … }`,
                // `get x () { … }` — but oxc's inner FunctionExpression span
                // starts at the `(`, so using it would make
                // `Function.prototype.toString` drop the key. The enclosing
                // ObjectProperty span is the right text.
                let method_span = sp(p.span);
                let with_span = |f: Function| Function { span: method_span, ..f };
                out.push(match p.kind {
                    ox::PropertyKind::Get => ObjectMember::Get {
                        key,
                        func: Box::new(with_span(lower_accessor_fn(&p.value)?)),
                    },
                    ox::PropertyKind::Set => ObjectMember::Set {
                        key,
                        func: Box::new(with_span(lower_accessor_fn(&p.value)?)),
                    },
                    ox::PropertyKind::Init if p.method => ObjectMember::Method {
                        key,
                        func: Box::new(with_span(lower_accessor_fn(&p.value)?)),
                    },
                    ox::PropertyKind::Init => ObjectMember::Prop {
                        key,
                        value: lower_expr(&p.value)?,
                        shorthand: p.shorthand,
                        // Always `None` from oxc — see NOTE 2. Nothing is
                        // dropped: oxc holds the initializer OUTSIDE the tree
                        // and has already either consumed it (destructuring) or
                        // raised the SyntaxError.
                        init: None,
                    },
                });
            }
        }
    }
    Ok(out)
}

/// oxc represents a method / getter / setter as an ordinary property whose
/// value is a `FunctionExpression`; ours names the shape directly.
fn lower_accessor_fn(v: &ox::Expression) -> R<Function> {
    match v {
        ox::Expression::FunctionExpression(f) => lower_function(f),
        _ => Err("unsupported: object method or accessor whose value is not a function".to_string()),
    }
}

// ----------------------------------------------------------------------------
// Template literals
// ----------------------------------------------------------------------------

fn lower_template(t: &ox::TemplateLiteral) -> R<TemplateLit> {
    // The consumer relies on `quasis.len() == exprs.len() + 1` to interleave
    // without bounds checks, so verify it here rather than trust it.
    if t.quasis.len() != t.expressions.len() + 1 {
        return Err(format!(
            "unsupported: malformed template ({} quasis, {} expressions)",
            t.quasis.len(),
            t.expressions.len()
        ));
    }
    let quasis: Vec<TemplateElement> = t.quasis.iter().map(lower_template_element).collect();
    let mut exprs = Vec::with_capacity(t.expressions.len());
    for e in &t.expressions {
        exprs.push(lower_expr(e)?);
    }
    Ok(TemplateLit { quasis, exprs })
}

fn lower_template_element(q: &ox::TemplateElement) -> TemplateElement {
    TemplateElement {
        // `None` survives as `None`: an invalid escape leaves the cooked value
        // `undefined`, which is legal in a TAGGED template and a SyntaxError
        // otherwise, and only the consumer knows which this is.
        cooked: q.value.cooked.as_ref().map(|c| str_val(c, q.lone_surrogates)),
        raw: q.value.raw.as_str().into(),
    }
}

// ----------------------------------------------------------------------------
// Operators
//
// Total mappings, so they are infallible: every oxc operator has a counterpart.
// ----------------------------------------------------------------------------

fn unary_op(op: ox::UnaryOperator) -> UnaryOp {
    use ox::UnaryOperator as O;
    match op {
        O::UnaryPlus => UnaryOp::Plus,
        O::UnaryNegation => UnaryOp::Minus,
        O::LogicalNot => UnaryOp::Not,
        O::BitwiseNot => UnaryOp::BitNot,
        O::Typeof => UnaryOp::Typeof,
        O::Void => UnaryOp::Void,
        O::Delete => UnaryOp::Delete,
    }
}

fn update_op(op: ox::UpdateOperator) -> UpdateOp {
    match op {
        ox::UpdateOperator::Increment => UpdateOp::Inc,
        ox::UpdateOperator::Decrement => UpdateOp::Dec,
    }
}

fn binary_op(op: ox::BinaryOperator) -> BinaryOp {
    use ox::BinaryOperator as O;
    match op {
        O::Addition => BinaryOp::Add,
        O::Subtraction => BinaryOp::Sub,
        O::Multiplication => BinaryOp::Mul,
        O::Division => BinaryOp::Div,
        O::Remainder => BinaryOp::Rem,
        O::Exponential => BinaryOp::Exp,
        O::Equality => BinaryOp::Eq,
        O::Inequality => BinaryOp::NotEq,
        O::StrictEquality => BinaryOp::StrictEq,
        O::StrictInequality => BinaryOp::StrictNotEq,
        O::LessThan => BinaryOp::Lt,
        O::LessEqualThan => BinaryOp::LtEq,
        O::GreaterThan => BinaryOp::Gt,
        O::GreaterEqualThan => BinaryOp::GtEq,
        O::ShiftLeft => BinaryOp::Shl,
        O::ShiftRight => BinaryOp::Shr,
        O::ShiftRightZeroFill => BinaryOp::UShr,
        O::BitwiseAnd => BinaryOp::BitAnd,
        O::BitwiseOR => BinaryOp::BitOr,
        O::BitwiseXOR => BinaryOp::BitXor,
        O::In => BinaryOp::In,
        O::Instanceof => BinaryOp::Instanceof,
    }
}

fn logical_op(op: ox::LogicalOperator) -> LogicalOp {
    match op {
        ox::LogicalOperator::And => LogicalOp::And,
        ox::LogicalOperator::Or => LogicalOp::Or,
        ox::LogicalOperator::Coalesce => LogicalOp::Coalesce,
    }
}

fn assign_op(op: ox::AssignmentOperator) -> AssignOp {
    use ox::AssignmentOperator as O;
    match op {
        O::Assign => AssignOp::Assign,
        O::Addition => AssignOp::Add,
        O::Subtraction => AssignOp::Sub,
        O::Multiplication => AssignOp::Mul,
        O::Division => AssignOp::Div,
        O::Remainder => AssignOp::Rem,
        O::Exponential => AssignOp::Exp,
        O::ShiftLeft => AssignOp::Shl,
        O::ShiftRight => AssignOp::Shr,
        O::ShiftRightZeroFill => AssignOp::UShr,
        O::BitwiseAnd => AssignOp::BitAnd,
        O::BitwiseOR => AssignOp::BitOr,
        O::BitwiseXOR => AssignOp::BitXor,
        // These short-circuit; they are not sugar for the arithmetic forms.
        O::LogicalAnd => AssignOp::LogicalAnd,
        O::LogicalOr => AssignOp::LogicalOr,
        O::LogicalNullish => AssignOp::LogicalCoalesce,
    }
}

// NOTE: `Target::Call` has NO arm below, because oxc has no path to it. Verified in
// oxc_parser-0.126.0/src/js/grammar.rs: `SimpleAssignmentTarget::cover` sends a
// `CallExpression` LHS straight to `p.fatal_error(invalid_assignment(..))`, and
// `SimpleAssignmentTarget` has no `CallExpression` variant to hold one anyway. So
// `f() = 1` never reaches this bridge — it is rejected before lowering, which is
// exactly why the engine still rewrites source text (`annexb_call_target_rewrite`).
// `Target::Call` only becomes reachable when the hand-written parser lands and
// builds it directly. Nothing here should be "fixed" to emit it.
//
// NOTE: `Target::Ident.covered` is ALWAYS `false` through this bridge, and cannot be
// otherwise. oxc's `AssignmentTarget` has no parenthesized variant; `cover` peels
// `Expression::ParenthesizedExpression` and discards the span. So `(x) = function(){}`
// lowers identically to `x = function(){}` and would wrongly get NamedEvaluation.
// This is a real fidelity gap, not an oversight — but it is invisible to `zipp bcdiff`
// only if the current oxc-based compiler loses the same bit, which it does (it reads
// the same AST). The real parser must set this.
//
// NOTE: `PropKey` cannot represent a BigInt key. `({1n: 1})` is legal — the spec's
// `NumericLiteral` production includes bigint literals — and its key is the exact
// decimal digits, which an f64 cannot round-trip (`{100000000000000000000000n:1}` would
// key as "1e+23"). Lowering it to `PropKey::Num` would be silently wrong, so it errors.
//
// NOTE: strings. oxc does NOT expose raw UTF-16. `ox::StringLiteral` carries
// `value: Str` (UTF-8) plus a `lone_surrogates: bool`; when that flag is set, `value` is
// oxc's marker encoding — U+FFFD followed by four hex digits per raw code unit, with a
// genuine U+FFFD written `\u{FFFD}fffd`. `str_lit` below decodes that back to code units
// and hands them to `StrVal::from_utf16`, which re-collapses to `Utf8` when well-formed.
//
// NOTE: `str_lit`/`decode_lone_surrogates` are SHARED — the expression/literal section
// needs byte-identical helpers for `Expr::Str` and template cooked values. Keep exactly
// one copy at assembly time; they are here because a property key needs them.



/// Undo oxc's lone-surrogate marker encoding.
///
/// Only called when oxc set `lone_surrogates`, so the input is known to be its own
/// output. The malformed branch is unreachable in practice but must not lose text: a
/// silently truncated string literal is a wrong program, not a crash.
fn decode_lone_surrogates(s: &str) -> StrVal {
    let mut units: Vec<u16> = Vec::with_capacity(s.len());
    let mut buf = [0u16; 2];
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\u{FFFD}' {
            units.extend_from_slice(c.encode_utf16(&mut buf));
            continue;
        }
        let hex: String = chars.by_ref().take(4).collect();
        match u16::from_str_radix(&hex, 16) {
            Ok(unit) => units.push(unit),
            Err(_) => {
                units.push(0xFFFD);
                for h in hex.chars() {
                    units.extend_from_slice(h.encode_utf16(&mut buf));
                }
            }
        }
    }
    StrVal::from_utf16(units)
}

// ============================================================================
// Binding patterns
// ============================================================================

fn lower_pattern(p: &ox::BindingPattern) -> R<Pattern> {
    use ox::BindingPattern as B;
    match p {
        B::BindingIdentifier(id) => Ok(Pattern::Ident(name(id.name.as_str()))),

        B::ArrayPattern(a) => {
            // oxc keeps the rest OUT of `elements`, in its own field. We put it back
            // where the grammar has it — last — so consumers can iterate one list.
            let mut elems: Vec<Option<PatternElem>> = Vec::with_capacity(a.elements.len() + 1);
            for el in &a.elements {
                elems.push(match el {
                    // A hole. Not `undefined`: `[[a] = [1]]` vs `[[a], ] = [[1]]` differ.
                    None => None,
                    Some(p) => Some(PatternElem { pat: lower_pattern(p)? }),
                });
            }
            if let Some(rest) = &a.rest {
                elems.push(Some(PatternElem {
                    pat: Pattern::Rest(Box::new(lower_pattern(&rest.argument)?)),
                }));
            }
            Ok(Pattern::Array(elems))
        }

        B::ObjectPattern(o) => {
            let mut props = Vec::with_capacity(o.properties.len());
            for prop in &o.properties {
                props.push(PatternProp {
                    key: lower_prop_key(&prop.key, prop.computed)?,
                    value: lower_pattern(&prop.value)?,
                    // `{a}` and `{a: a}` bind the same thing but are not
                    // interchangeable to the parser's early errors, so it is carried
                    // rather than recomputed from key/value equality.
                    shorthand: prop.shorthand,
                });
            }
            // `Pattern::Object.rest` is already named `rest`, so there is no
            // `Pattern::Rest` wrapper here — that would say it twice.
            let rest = match &o.rest {
                Some(r) => Some(Box::new(lower_pattern(&r.argument)?)),
                None => None,
            };
            Ok(Pattern::Object { props, rest })
        }

        B::AssignmentPattern(a) => Ok(Pattern::Assign {
            left: Box::new(lower_pattern(&a.left)?),
            right: Box::new(lower_expr(&a.right)?),
        }),
    }
}

// ============================================================================
// Assignment targets
// ============================================================================

fn lower_target(t: &ox::AssignmentTarget) -> R<Target> {
    use ox::AssignmentTarget as T;
    match t {
        T::ArrayAssignmentTarget(a) => {
            let mut elems: Vec<Option<TargetElem>> = Vec::with_capacity(a.elements.len() + 1);
            for el in &a.elements {
                elems.push(match el {
                    None => None,
                    Some(e) => {
                        let (target, default) = lower_target_maybe_default(e)?;
                        Some(TargetElem { target, default, rest: false })
                    }
                });
            }
            // As with `ArrayPattern`, oxc parks the rest beside the elements. It is
            // always last (the parser rejects anything after it), so appending is
            // faithful. `[...a = 1]` is a SyntaxError, hence `default: None`.
            if let Some(rest) = &a.rest {
                elems.push(Some(TargetElem {
                    target: lower_target(&rest.target)?,
                    default: None,
                    rest: true,
                }));
            }
            Ok(Target::Array(elems))
        }

        T::ObjectAssignmentTarget(o) => {
            let mut props = Vec::with_capacity(o.properties.len());
            for p in &o.properties {
                props.push(lower_target_prop(p)?);
            }
            let rest = match &o.rest {
                Some(r) => Some(Box::new(lower_target(&r.target)?)),
                None => None,
            };
            Ok(Target::Object { props, rest })
        }

        // Everything left is a `SimpleAssignmentTarget` by construction — the two
        // pattern variants above are the whole of `AssignmentTargetPattern`.
        other => match other.as_simple_assignment_target() {
            Some(s) => lower_simple_target(s),
            None => Err("unsupported: assignment target".to_string()),
        },
    }
}

fn lower_simple_target(t: &ox::SimpleAssignmentTarget) -> R<Target> {
    use ox::SimpleAssignmentTarget as S;
    // Exhaustive on purpose: a new oxc variant should break the build rather than
    // fall into a catch-all that guesses.
    match t {
        S::AssignmentTargetIdentifier(id) => Ok(Target::Ident {
            name: name(id.name.as_str()),
            // See the NOTE at the top: oxc discarded the parens before we got here.
            covered: false,
        }),

        S::ComputedMemberExpression(m) => Ok(Target::Member(Box::new(Member {
            object: lower_expr(&m.object)?,
            prop: MemberProp::Computed(lower_expr(&m.expression)?),
            optional: m.optional,
        }))),
        S::StaticMemberExpression(m) => Ok(Target::Member(Box::new(Member {
            object: lower_expr(&m.object)?,
            prop: MemberProp::Ident(name(m.property.name.as_str())),
            optional: m.optional,
        }))),
        S::PrivateFieldExpression(m) => Ok(Target::Member(Box::new(Member {
            object: lower_expr(&m.object)?,
            prop: MemberProp::Private(name(m.field.name.as_str())),
            optional: m.optional,
        }))),

        S::TSAsExpression(_)
        | S::TSSatisfiesExpression(_)
        | S::TSNonNullExpression(_)
        | S::TSTypeAssertion(_) => {
            Err("unsupported: TypeScript expression as assignment target".to_string())
        }
    }
}

/// `x` or `x = init` in a destructuring assignment.
///
/// Returns the two pieces rather than a `TargetElem` because an object property needs
/// exactly the same pair and stores it in a different struct.
fn lower_target_maybe_default(t: &ox::AssignmentTargetMaybeDefault) -> R<(Target, Option<Expr>)> {
    use ox::AssignmentTargetMaybeDefault as M;
    match t {
        M::AssignmentTargetWithDefault(d) => {
            Ok((lower_target(&d.binding)?, Some(lower_expr(&d.init)?)))
        }
        other => match other.as_assignment_target() {
            Some(t) => Ok((lower_target(t)?, None)),
            None => Err("unsupported: destructuring assignment element".to_string()),
        },
    }
}

fn lower_target_prop(p: &ox::AssignmentTargetProperty) -> R<TargetProp> {
    use ox::AssignmentTargetProperty as P;
    match p {
        // `({a} = o)` and `({a = 1} = o)`. oxc keeps the shorthand form as a bare
        // reference with the CoverInitializedName's initializer beside it, so key and
        // target are the same identifier and there is nothing to derive.
        P::AssignmentTargetPropertyIdentifier(id) => {
            let n = name(id.binding.name.as_str());
            Ok(TargetProp {
                key: PropKey::Ident(n.clone()),
                target: Target::Ident { name: n, covered: false },
                default: match &id.init {
                    Some(e) => Some(lower_expr(e)?),
                    None => None,
                },
                shorthand: true,
            })
        }
        // `({k: t} = o)` / `({k: t = 1} = o)` / `({[k]: t} = o)`. The default, when
        // present, is inside `binding` as an `AssignmentTargetWithDefault`.
        P::AssignmentTargetPropertyProperty(pp) => {
            let (target, default) = lower_target_maybe_default(&pp.binding)?;
            Ok(TargetProp {
                key: lower_prop_key(&pp.name, pp.computed)?,
                target,
                default,
                shorthand: false,
            })
        }
    }
}

// ============================================================================
// Property keys
// ============================================================================

fn lower_prop_key(k: &ox::PropertyKey, computed: bool) -> R<PropKey> {
    use ox::PropertyKey as K;

    // `computed` decides the shape BEFORE the literal form does, and the difference is
    // observable: `{__proto__: v}` and `{"__proto__": v}` set the prototype while
    // `{["__proto__"]: v}` defines an own property, and a class's computed key is
    // evaluated at class-definition time. Collapsing `["a"]` to `PropKey::Str` would
    // lose both.
    if computed {
        return match k.as_expression() {
            Some(e) => Ok(PropKey::Computed(lower_expr(e)?)),
            // `#x` and a bare `IdentifierName` are the only non-expression keys and
            // neither can be computed, so this is oxc contradicting itself.
            None => Err("unsupported: computed property key that is not an expression".to_string()),
        };
    }

    match k {
        K::StaticIdentifier(id) => Ok(PropKey::Ident(name(id.name.as_str()))),
        K::PrivateIdentifier(id) => Ok(PropKey::Private(name(id.name.as_str()))),
        K::StringLiteral(s) => Ok(PropKey::Str(str_lit(s))),
        // Only the value: the property key is the CANONICAL number-to-string, which
        // belongs to the consumer that already owns the engine's conversion.
        K::NumericLiteral(n) => Ok(PropKey::Num(n.value)),
        // See the NOTE: legal syntax the target AST cannot say.
        K::BigIntLiteral(_) => Err("unsupported: BigInt property key".to_string()),
        // A non-computed key is `IdentifierName | StringLiteral | NumericLiteral |
        // PrivateIdentifier` and nothing else. Falling back to `Computed` here would
        // change `__proto__` semantics, so refuse instead.
        _ => Err("unsupported: non-computed property key that is not a literal name".to_string()),
    }
}

// ============================================================================
// Parameters
// ============================================================================

fn lower_params(p: &ox::FormalParameters) -> R<Params> {
    let mut items: Vec<Pattern> = Vec::with_capacity(p.items.len() + 1);

    for item in &p.items {
        if !item.decorators.is_empty() {
            return Err("unsupported: parameter decorator".to_string());
        }
        let pat = lower_pattern(&item.pattern)?;
        // oxc hangs a parameter's default off `FormalParameter::initializer` instead of
        // wrapping the pattern (its own docs say `AssignmentPattern` is invalid in a
        // `FormalParameter`). Re-attach it: `Pattern::Assign` is the one place the rest
        // of the AST looks, and `simple` below depends on it being visible there.
        items.push(match &item.initializer {
            Some(init) => {
                Pattern::Assign { left: Box::new(pat), right: Box::new(lower_expr(init)?) }
            }
            None => pat,
        });
    }

    // Same rest-is-a-side-channel shape as the array forms.
    if let Some(rest) = &p.rest {
        if !rest.decorators.is_empty() {
            return Err("unsupported: parameter decorator".to_string());
        }
        items.push(Pattern::Rest(Box::new(lower_pattern(&rest.rest.argument)?)));
    }

    // IsSimpleParameterList. Not a convenience flag: it decides whether `arguments` is
    // mapped, whether a "use strict" directive in the body is a SyntaxError, and
    // whether the parameters get their own scope. An empty list IS simple.
    let simple = items.iter().all(|i| matches!(i, Pattern::Ident(_)));

    Ok(Params { items, simple })
}

// NOTE: `lower_export` — I do not own the modules section, so I had to pick a call
// shape. All three export statement variants are reached here through
// `ox::Statement::as_module_declaration()`, which oxc generates for the
// `inherit_variants!` flattening, so this section assumes
// `fn lower_export(d: &ox::ModuleDeclaration) -> R<ExportDecl>`. That is the only
// single-function signature that can cover Named/Default/All; if the modules
// section instead lands three functions, the three arms below are the only call
// sites to change.
// NOTE: `using` / `await using` ARE modelled by oxc 0.126 —
// `ox::VariableDeclarationKind::{Using, AwaitUsing}` (js.rs:1248) — so `VarKind`
// maps one-to-one with nothing left over.
// NOTE: directive raw text is `ox::Directive::directive` (a `Str<'a>`, js.rs:1166),
// documented as "raw content of directive as it appears in source, any escapes
// left as is", and it excludes the quotes — oxc's own `Directive::is_use_strict`
// is `self.directive == "use strict"`. That is exactly the comparison our
// `Directive.raw` exists to allow, so `"use\u0020strict"` correctly does not
// count.
// NOTE: strings — oxc DOES expose lone surrogates, but not as UTF-16. A
// `StringLiteral` carries `lone_surrogates: bool` (literal.rs:109) and, when set,
// `value` is a marker encoding: U+FFFD followed by four hex digits per lone
// surrogate, with a genuine U+FFFD written `\u{FFFD}fffd`. `directive_str_val`
// below undoes that and feeds `StrVal::from_utf16`. If the expressions section
// lands a shared string-literal helper, delete `directive_str_val` and call that
// instead — the decode is identical.
// NOTE: `ox::Program::hashbang` has no home in our `Program`. Not lowered, and
// not an error: `#!` is a HashbangComment, i.e. trivia, not a node.
// NOTE: TS-only fields (`VariableDeclaration::declare`, `VariableDeclarator::
// {type_annotation, definite}`, `CatchParameter::type_annotation`) are not
// inspected — they cannot be populated under a JS source type. The TS-only
// *statement* variants are rejected explicitly rather than swept into a
// catch-all, so an oxc bump that adds a variant fails to compile here.

pub fn lower_program(p: &ox::Program, goal: Goal) -> R<Program> {
    let directives = lower_directives(&p.directives);
    // Only the two sources the AST can actually see. A strict eval CALLER also
    // forces strictness, but that fact lives with the caller, not the source, so
    // it is ORed in by whoever has it rather than guessed at here.
    let strict = goal == Goal::Module || directives.iter().any(|d| &*d.raw == "use strict");
    let body = with_strict(strict, || lower_stmts(&p.body))?;
    Ok(Program { goal, strict, directives, body })
}

fn lower_stmts(v: &[ox::Statement]) -> R<Vec<Stmt>> {
    v.iter().map(lower_stmt).collect()
}

fn lower_stmt(s: &ox::Statement) -> R<Stmt> {
    Ok(match s {
        ox::Statement::BlockStatement(b) => Stmt::Block(lower_stmts(&b.body)?),
        ox::Statement::EmptyStatement(_) => Stmt::Empty,
        ox::Statement::ExpressionStatement(e) => Stmt::Expr(lower_expr(&e.expression)?),

        ox::Statement::IfStatement(i) => Stmt::If {
            test: lower_expr(&i.test)?,
            cons: Box::new(lower_stmt(&i.consequent)?),
            alt: i.alternate.as_ref().map(lower_stmt).transpose()?.map(Box::new),
        },

        ox::Statement::WhileStatement(w) => {
            Stmt::While { test: lower_expr(&w.test)?, body: Box::new(lower_stmt(&w.body)?) }
        }
        ox::Statement::DoWhileStatement(d) => {
            Stmt::DoWhile { body: Box::new(lower_stmt(&d.body)?), test: lower_expr(&d.test)? }
        }

        ox::Statement::ForStatement(f) => Stmt::For {
            init: f.init.as_ref().map(lower_for_init).transpose()?,
            test: lower_opt_expr(&f.test)?,
            update: lower_opt_expr(&f.update)?,
            body: Box::new(lower_stmt(&f.body)?),
        },
        ox::Statement::ForInStatement(f) => Stmt::ForIn {
            left: lower_for_target(&f.left)?,
            right: lower_expr(&f.right)?,
            body: Box::new(lower_stmt(&f.body)?),
        },
        ox::Statement::ForOfStatement(f) => Stmt::ForOf {
            left: lower_for_target(&f.left)?,
            right: lower_expr(&f.right)?,
            body: Box::new(lower_stmt(&f.body)?),
            is_await: f.r#await,
        },

        ox::Statement::SwitchStatement(s) => {
            let mut cases = Vec::with_capacity(s.cases.len());
            for c in &s.cases {
                // `test: None` is `default:`, which is the same convention ours
                // uses, so no marker is needed.
                cases
                    .push(SwitchCase { test: lower_opt_expr(&c.test)?, body: lower_stmts(&c.consequent)? });
            }
            Stmt::Switch { disc: lower_expr(&s.discriminant)?, cases }
        }

        ox::Statement::ReturnStatement(r) => Stmt::Return(lower_opt_expr(&r.argument)?),
        ox::Statement::ThrowStatement(t) => Stmt::Throw(lower_expr(&t.argument)?),

        ox::Statement::TryStatement(t) => {
            let handler = match &t.handler {
                Some(c) => Some(CatchClause {
                    // `catch {}` — optional catch binding — is `param: None` on
                    // both sides, so the absence survives rather than becoming a
                    // synthetic name.
                    param: match &c.param {
                        Some(p) => Some(lower_pattern(&p.pattern)?),
                        None => None,
                    },
                    body: lower_stmts(&c.body.body)?,
                }),
                None => None,
            };
            Stmt::Try {
                block: lower_stmts(&t.block.body)?,
                handler,
                finalizer: t.finalizer.as_ref().map(|b| lower_stmts(&b.body)).transpose()?,
            }
        }

        ox::Statement::BreakStatement(b) => {
            Stmt::Break(b.label.as_ref().map(|l| name(l.name.as_str())))
        }
        ox::Statement::ContinueStatement(c) => {
            Stmt::Continue(c.label.as_ref().map(|l| name(l.name.as_str())))
        }
        ox::Statement::LabeledStatement(l) => Stmt::Labeled {
            label: name(l.label.name.as_str()),
            body: Box::new(lower_stmt(&l.body)?),
        },

        ox::Statement::WithStatement(w) => {
            Stmt::With { object: lower_expr(&w.object)?, body: Box::new(lower_stmt(&w.body)?) }
        }
        ox::Statement::DebuggerStatement(_) => Stmt::Debugger,

        // oxc flattens `Declaration` into `Statement`, so these arrive here
        // rather than under one `Declaration` variant.
        ox::Statement::VariableDeclaration(d) => Stmt::VarDecl(lower_var_decl(d)?),
        ox::Statement::FunctionDeclaration(f) => Stmt::FnDecl(Box::new(lower_function(f)?)),
        ox::Statement::ClassDeclaration(c) => Stmt::ClassDecl(Box::new(lower_class(c)?)),

        // `ModuleDeclaration` is flattened the same way. The export variants go
        // back through `as_module_declaration` so the modules section sees one
        // node type; the accessor cannot fail in these arms, but it returns an
        // `Option` and this file does not unwrap.
        ox::Statement::ImportDeclaration(d) => Stmt::Import(Box::new(lower_import(d)?)),
        ox::Statement::ExportAllDeclaration(_)
        | ox::Statement::ExportDefaultDeclaration(_)
        | ox::Statement::ExportNamedDeclaration(_) => {
            let m = s
                .as_module_declaration()
                .ok_or_else(|| "unsupported: export statement is not a module declaration".to_string())?;
            Stmt::Export(Box::new(lower_export(m)?))
        }

        ox::Statement::TSTypeAliasDeclaration(_) => return Err("unsupported: TS type alias".into()),
        ox::Statement::TSInterfaceDeclaration(_) => return Err("unsupported: TS interface".into()),
        ox::Statement::TSEnumDeclaration(_) => return Err("unsupported: TS enum".into()),
        ox::Statement::TSModuleDeclaration(_) => return Err("unsupported: TS module/namespace".into()),
        ox::Statement::TSGlobalDeclaration(_) => return Err("unsupported: TS global declaration".into()),
        ox::Statement::TSImportEqualsDeclaration(_) => {
            return Err("unsupported: TS import-equals declaration".into())
        }
        ox::Statement::TSExportAssignment(_) => return Err("unsupported: TS export assignment".into()),
        ox::Statement::TSNamespaceExportDeclaration(_) => {
            return Err("unsupported: TS namespace export".into())
        }
    })
}

fn lower_var_decl(d: &ox::VariableDeclaration) -> R<VarDecl> {
    let kind = match d.kind {
        ox::VariableDeclarationKind::Var => VarKind::Var,
        ox::VariableDeclarationKind::Let => VarKind::Let,
        ox::VariableDeclarationKind::Const => VarKind::Const,
        ox::VariableDeclarationKind::Using => VarKind::Using,
        ox::VariableDeclarationKind::AwaitUsing => VarKind::AwaitUsing,
    };
    let mut decls = Vec::with_capacity(d.declarations.len());
    for v in &d.declarations {
        decls.push(Declarator { id: lower_pattern(&v.id)?, init: lower_opt_expr(&v.init)? });
    }
    Ok(VarDecl { kind, decls })
}

/// `for (<init>; ...; ...)`. The declaration/expression split is the same on both
/// sides; everything oxc inherits from `Expression` here goes through
/// `as_expression`.
fn lower_for_init(i: &ox::ForStatementInit) -> R<ForInit> {
    if let ox::ForStatementInit::VariableDeclaration(d) = i {
        return Ok(ForInit::Var(lower_var_decl(d)?));
    }
    let e = i
        .as_expression()
        .ok_or_else(|| "unsupported: for-statement init is neither a declaration nor an expression".to_string())?;
    Ok(ForInit::Expr(lower_expr(e)?))
}

/// `for (<left> in/of ...)`. Either a fresh binding or an assignment to something
/// that already exists — `for ([a.b] of xs)` is legal, which is why the second
/// case is a `Target` and not a `Pattern`.
fn lower_for_target(l: &ox::ForStatementLeft) -> R<ForTarget> {
    if let ox::ForStatementLeft::VariableDeclaration(d) = l {
        return Ok(ForTarget::Var(lower_var_decl(d)?));
    }
    let t = l
        .as_assignment_target()
        .ok_or_else(|| "unsupported: for-in/of head is neither a declaration nor an assignment target".to_string())?;
    Ok(ForTarget::Target(lower_target(t)?))
}

/// The prologue. Infallible: a directive is a string literal and nothing else,
/// so there is no node here that can fail to lower.
fn lower_directives(d: &[ox::Directive]) -> Vec<Directive> {
    d.iter()
        .map(|d| Directive {
            // `d.directive` is the source text with escapes intact, which is what
            // decides use-strict; `d.expression.value` is the cooked value, which
            // does not.
            raw: d.directive.as_str().into(),
            value: directive_str_val(&d.expression),
        })
        .collect()
}

/// oxc hides lone surrogates behind a marker encoding rather than handing back
/// UTF-16, so `lone_surrogates` has to be undone before the value means anything.
fn directive_str_val(lit: &ox::StringLiteral) -> StrVal {
    let s = lit.value.as_str();
    if !lit.lone_surrogates {
        return StrVal::Utf8(s.to_string());
    }
    // Encoding: U+FFFD is the escape character. `\u{FFFD}XXXX` is the code unit
    // `XXXX`; a literal U+FFFD is written `\u{FFFD}fffd`.
    let mut units: Vec<u16> = Vec::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\u{FFFD}' {
            let mut buf = [0u16; 2];
            units.extend_from_slice(c.encode_utf16(&mut buf));
            continue;
        }
        let hex: String = chars.by_ref().take(4).collect();
        match u16::from_str_radix(&hex, 16) {
            Ok(u) => units.push(u),
            // Not an escape oxc produced. Keep the text verbatim rather than
            // dropping it — a malformed marker is a bug on their side, not
            // licence to lose characters.
            Err(_) => {
                units.push(0xFFFD);
                units.extend(hex.encode_utf16());
            }
        }
    }
    StrVal::from_utf16(units)
}

// NOTE: `str_lit` below is the only helper I had to invent — the shared contract
// lists no string-lowering helper, but imports/exports need one for module
// specifiers, import attributes and `export { x as "a b" }`. If the literals
// section defines the same thing, keep one copy and delete the other.
//
// NOTE: strictness cannot be fully computed here. The fixed signature
// `lower_function(&ox::Function) -> R<Function>` carries no *inherited*
// strictness, so `FnBody.strict` is true only for a body with its own
// `"use strict"` prologue, or for a class member (which I lower through an
// internal `lower_function_at` that forces it). A plain function nested inside a
// strict function therefore reports `strict: false`. This is not a regression:
// `compile/funcs.rs` already tracks `cx.in_strict` itself and ORs it with the
// body prologue, so the retarget stays a no-op. Whoever writes `lower_program`
// has the same gap for `Goal::Module` bodies.
//
// NOTE: `WithClauseKeyword::{With, Assert}` is dropped — `ImportAttribute` has no
// field for it. `assert { … }` is gone from the spec and `compile/string_accum.rs
// ::with_clause_type` already ignores the keyword, so this loses nothing today.
//
// NOTE: `accessor x = 1` (ClassElement::AccessorProperty) and decorators have no
// AST variant, so they are `Err`, not invented.



// ============================================================================
// Functions
// ============================================================================

fn lower_function(f: &ox::Function) -> R<Function> {
    // For a free function the node's own span IS its `toString` text.
    lower_function_at(f, sp(f.span), false)
}

/// Shared body of function lowering.
///
/// `span` is the text `Function.prototype.toString` must reproduce, which is not
/// always `f.span` — see `lower_class_element`. `strict` is strictness inherited
/// from the surrounding construct; the body's own prologue can only add to it.
fn lower_function_at(f: &ox::Function, span: Span, strict: bool) -> R<Function> {
    match f.r#type {
        ox::FunctionType::FunctionDeclaration | ox::FunctionType::FunctionExpression => {}
        // `declare function f(): void` and overload signatures: TypeScript, and
        // bodiless, so there is nothing to lower even in principle.
        other => return Err(format!("unsupported: {other:?} (TypeScript)")),
    }
    if f.type_parameters.is_some() || f.return_type.is_some() || f.this_param.is_some() || f.declare
    {
        return Err("unsupported: TypeScript annotations on a function".to_string());
    }
    let body = f.body.as_ref().ok_or("unsupported: function without a body")?;
    Ok(Function {
        name: f.id.as_ref().map(|id| name(id.name.as_str())),
        params: lower_params(&f.params)?,
        body: lower_fn_body(body, strict)?,
        is_async: f.r#async,
        is_generator: f.generator,
        span,
    })
}

fn lower_fn_body(b: &ox::FunctionBody, strict: bool) -> R<FnBody> {
    // `strict` is what the construct forces (a class member); the cell carries
    // what is inherited (an enclosing strict program or function); the prologue
    // is the body's own. oxc's `has_use_strict_directive` compares the RAW
    // directive text, so `"use strict"` correctly does not count.
    let s = strict || STRICT.with(|c| c.get()) || b.has_use_strict_directive();
    Ok(FnBody {
        directives: lower_directives(&b.directives),
        stmts: with_strict(s, || lower_stmts(&b.statements))?,
        strict: s,
    })
}

fn lower_arrow(a: &ox::ArrowFunctionExpression) -> R<Arrow> {
    if a.type_parameters.is_some() || a.return_type.is_some() {
        return Err("unsupported: TypeScript annotations on an arrow function".to_string());
    }
    // oxc funnels BOTH arrow body forms through `FunctionBody`; `expression` is
    // the only thing that separates `x => x + 1` from `x => { x + 1 }`. For the
    // expression form the parser builds a body of exactly one
    // `ExpressionStatement` and no directives, so `x => "use strict"` is an
    // expression, not a prologue — matching it structurally keeps that true.
    let body = if a.expression {
        match a.body.statements.first() {
            Some(ox::Statement::ExpressionStatement(e)) if a.body.statements.len() == 1 => {
                ArrowBody::Expr(Box::new(lower_expr(&e.expression)?))
            }
            _ => return Err("unsupported: arrow expression body is not a lone expression".to_string()),
        }
    } else {
        ArrowBody::Block(lower_fn_body(&a.body, false)?)
    };
    Ok(Arrow { params: lower_params(&a.params)?, body, is_async: a.r#async, span: sp(a.span) })
}

// ============================================================================
// Classes
// ============================================================================

fn lower_class(c: &ox::Class) -> R<Class> {
    if !c.decorators.is_empty() {
        return Err("unsupported: class decorators".to_string());
    }
    if c.r#abstract
        || c.declare
        || c.type_parameters.is_some()
        || c.super_type_arguments.is_some()
        || !c.implements.is_empty()
    {
        return Err("unsupported: TypeScript class modifiers".to_string());
    }
    // A class is strict code in its entirety — heritage expression included.
    let (body, superclass) = with_strict(true, || {
        let mut body = Vec::with_capacity(c.body.body.len());
        for el in &c.body.body {
            body.push(lower_class_element(el)?);
        }
        let superclass = match &c.super_class {
            Some(e) => Some(Box::new(lower_expr(e)?)),
            None => None,
        };
        Ok((body, superclass))
    })?;
    Ok(Class {
        name: c.id.as_ref().map(|id| name(id.name.as_str())),
        superclass,
        body,
        span: sp(c.span),
    })
}

fn lower_class_element(el: &ox::ClassElement) -> R<ClassMember> {
    match el {
        ox::ClassElement::MethodDefinition(m) => {
            if !m.decorators.is_empty() {
                return Err("unsupported: method decorators".to_string());
            }
            if !matches!(m.r#type, ox::MethodDefinitionType::MethodDefinition)
                || m.r#override
                || m.optional
                || m.accessibility.is_some()
            {
                return Err("unsupported: TypeScript method modifiers".to_string());
            }
            // SPAN, and this is the fiddly one. A method's [[SourceText]] is the
            // MethodDefinition production: key included, `get`/`set`/`async`/`*`
            // included, `static` EXCLUDED (that belongs to ClassElement). oxc's
            // value-`Function` span starts at the `(`, so it is unusable; `m.span`
            // is right except that it also covers a leading `static` and the
            // trivia after it. `m.span` is handed over unchanged because it is
            // precisely what the compiler consumes today —
            // `compile/mod.rs::method_source` strips `static` and
            // `skip_leading_trivia` the comments/whitespace behind it — so the
            // retarget stays a no-op. A span cannot encode "minus the `static`";
            // if that trimming ever moves off the consumer it has to move here,
            // and it needs the source text to do it.
            Ok(ClassMember::Method(ClassMethod {
                key: lower_prop_key(&m.key, m.computed)?,
                kind: match m.kind {
                    ox::MethodDefinitionKind::Constructor => MethodKind::Constructor,
                    ox::MethodDefinitionKind::Method => MethodKind::Method,
                    ox::MethodDefinitionKind::Get => MethodKind::Get,
                    ox::MethodDefinitionKind::Set => MethodKind::Set,
                },
                // A class body is always strict code, prologue or not.
                func: Box::new(lower_function_at(&m.value, sp(m.span), true)?),
                is_static: m.r#static,
            }))
        }
        ox::ClassElement::PropertyDefinition(p) => {
            if !p.decorators.is_empty() {
                return Err("unsupported: field decorators".to_string());
            }
            if !matches!(p.r#type, ox::PropertyDefinitionType::PropertyDefinition)
                || p.declare
                || p.r#override
                || p.optional
                || p.definite
                || p.readonly
                || p.accessibility.is_some()
                || p.type_annotation.is_some()
            {
                return Err("unsupported: TypeScript field modifiers".to_string());
            }
            Ok(ClassMember::Field(ClassField {
                key: lower_prop_key(&p.key, p.computed)?,
                value: lower_opt_expr(&p.value)?,
                is_static: p.r#static,
            }))
        }
        // A static block's statements are strict, but `ClassMember::StaticBlock`
        // holds bare statements with nowhere to record that — the consumer knows
        // it from the position, as `compile/funcs.rs` already does.
        ox::ClassElement::StaticBlock(b) => Ok(ClassMember::StaticBlock(lower_stmts(&b.body)?)),
        ox::ClassElement::AccessorProperty(_) => {
            Err("unsupported: class `accessor` property".to_string())
        }
        ox::ClassElement::TSIndexSignature(_) => {
            Err("unsupported: TypeScript index signature".to_string())
        }
    }
}

// ============================================================================
// Modules
// ============================================================================

/// oxc has no `Evaluation` variant: the ordinary phase is the absence of one.
fn lower_import_phase(p: Option<ox::ImportPhase>) -> ImportPhase {
    match p {
        None => ImportPhase::Evaluation,
        Some(ox::ImportPhase::Source) => ImportPhase::Source,
        Some(ox::ImportPhase::Defer) => ImportPhase::Defer,
    }
}

/// `with { type: "json" }` (and the legacy `assert { … }`, whose keyword we do
/// not record — see the NOTE at the top).
fn lower_with_clause(wc: Option<&ox::WithClause>) -> Vec<ImportAttribute> {
    let Some(wc) = wc else { return Vec::new() };
    wc.with_entries
        .iter()
        .map(|a| ImportAttribute {
            key: match &a.key {
                ox::ImportAttributeKey::Identifier(id) => {
                    StrVal::Utf8(id.name.as_str().to_string())
                }
                ox::ImportAttributeKey::StringLiteral(s) => str_lit(s),
            },
            value: str_lit(&a.value),
        })
        .collect()
}

/// oxc splits an identifier spelling into "name" and "reference" depending on
/// which side of the `as` it sits on; the distinction is a binding concern this
/// AST deliberately does not carry, so both collapse to `Ident`.
fn lower_module_export_name(n: &ox::ModuleExportName) -> ModuleExportName {
    match n {
        ox::ModuleExportName::IdentifierName(i) => ModuleExportName::Ident(name(i.name.as_str())),
        ox::ModuleExportName::IdentifierReference(i) => {
            ModuleExportName::Ident(name(i.name.as_str()))
        }
        ox::ModuleExportName::StringLiteral(s) => ModuleExportName::Str(str_lit(s)),
    }
}

fn lower_import(d: &ox::ImportDeclaration) -> R<ImportDecl> {
    if matches!(d.import_kind, ox::ImportOrExportKind::Type) {
        return Err("unsupported: `import type` (TypeScript)".to_string());
    }
    let mut specifiers = Vec::new();
    // `specifiers: None` is `import "m"` — a side-effect import, which is not the
    // same as `import {} from "m"`. Both flatten to an empty list here because
    // the difference is not observable once the module is linked.
    for s in d.specifiers.iter().flatten() {
        specifiers.push(match s {
            ox::ImportDeclarationSpecifier::ImportDefaultSpecifier(s) => {
                ImportSpecifier::Default(name(s.local.name.as_str()))
            }
            ox::ImportDeclarationSpecifier::ImportNamespaceSpecifier(s) => {
                ImportSpecifier::Namespace(name(s.local.name.as_str()))
            }
            ox::ImportDeclarationSpecifier::ImportSpecifier(s) => {
                if matches!(s.import_kind, ox::ImportOrExportKind::Type) {
                    return Err("unsupported: `import { type X }` (TypeScript)".to_string());
                }
                ImportSpecifier::Named {
                    imported: lower_module_export_name(&s.imported),
                    local: name(s.local.name.as_str()),
                }
            }
        });
    }
    Ok(ImportDecl {
        specifiers,
        source: str_lit(&d.source),
        attributes: lower_with_clause(d.with_clause.as_deref()),
        phase: lower_import_phase(d.phase),
    })
}

/// Every export form. `lower_stmt` sees these as three separate `Statement`
/// variants (`inherit_variants!` flattens `ModuleDeclaration` into `Statement`),
/// so the three functions below are the real entry points; this one exists for a
/// caller that holds an undestructured `ModuleDeclaration`.
fn lower_export(d: &ox::ModuleDeclaration) -> R<ExportDecl> {
    match d {
        ox::ModuleDeclaration::ExportNamedDeclaration(e) => lower_export_named(e),
        ox::ModuleDeclaration::ExportDefaultDeclaration(e) => lower_export_default(e),
        ox::ModuleDeclaration::ExportAllDeclaration(e) => lower_export_all(e),
        ox::ModuleDeclaration::ImportDeclaration(_) => {
            Err("unsupported: import reached export lowering".to_string())
        }
        ox::ModuleDeclaration::TSExportAssignment(_) => {
            Err("unsupported: `export =` (TypeScript)".to_string())
        }
        ox::ModuleDeclaration::TSNamespaceExportDeclaration(_) => {
            Err("unsupported: `export as namespace` (TypeScript)".to_string())
        }
    }
}

fn lower_export_named(e: &ox::ExportNamedDeclaration) -> R<ExportDecl> {
    if matches!(e.export_kind, ox::ImportOrExportKind::Type) {
        return Err("unsupported: `export type` (TypeScript)".to_string());
    }
    // `export <decl>` and `export { … }` are grammatically exclusive, and the
    // declaration form has no specifiers and no source — so a declaration
    // decides the variant outright.
    if let Some(decl) = &e.declaration {
        return Ok(ExportDecl::Decl(Box::new(lower_exported_decl(decl)?)));
    }
    let mut specifiers = Vec::with_capacity(e.specifiers.len());
    for s in &e.specifiers {
        if matches!(s.export_kind, ox::ImportOrExportKind::Type) {
            return Err("unsupported: `export { type X }` (TypeScript)".to_string());
        }
        specifiers.push(ExportSpecifier {
            local: lower_module_export_name(&s.local),
            exported: lower_module_export_name(&s.exported),
        });
    }
    Ok(ExportDecl::Named {
        specifiers,
        source: e.source.as_ref().map(str_lit),
        attributes: lower_with_clause(e.with_clause.as_deref()),
    })
}

/// The declaration inside `export var/let/const/function/class …`, lowered to
/// the statement it would have been without the `export` — which is what the
/// binding and hoisting rules say it still is.
fn lower_exported_decl(d: &ox::Declaration) -> R<Stmt> {
    match d {
        ox::Declaration::VariableDeclaration(v) => Ok(Stmt::VarDecl(lower_var_decl(v)?)),
        ox::Declaration::FunctionDeclaration(f) => Ok(Stmt::FnDecl(Box::new(lower_function(f)?))),
        ox::Declaration::ClassDeclaration(c) => Ok(Stmt::ClassDecl(Box::new(lower_class(c)?))),
        _ => Err("unsupported: TypeScript declaration under `export`".to_string()),
    }
}

fn lower_export_default(e: &ox::ExportDefaultDeclaration) -> R<ExportDecl> {
    use ox::ExportDefaultDeclarationKind as K;
    let d = match &e.declaration {
        // `export default function …` / `class …` are DECLARATIONS, not
        // expressions — the function hoists, and an anonymous one is named
        // "default" by NamedEvaluation. Collapsing them into `Expr` would lose
        // the hoisting, so the three-way split is load-bearing.
        K::FunctionDeclaration(f) => ExportDefault::Function(Box::new(lower_function(f)?)),
        K::ClassDeclaration(c) => ExportDefault::Class(Box::new(lower_class(c)?)),
        K::TSInterfaceDeclaration(_) => {
            return Err("unsupported: `export default interface` (TypeScript)".to_string());
        }
        // Everything else is an AssignmentExpression, inherited into this enum
        // from `Expression` — `as_expression` is the accessor that inheritance
        // generates.
        other => {
            let expr = other.as_expression().ok_or("unsupported: export default declaration")?;
            ExportDefault::Expr(lower_expr(expr)?)
        }
    };
    Ok(ExportDecl::Default(d))
}

fn lower_export_all(e: &ox::ExportAllDeclaration) -> R<ExportDecl> {
    if matches!(e.export_kind, ox::ImportOrExportKind::Type) {
        return Err("unsupported: `export type *` (TypeScript)".to_string());
    }
    Ok(ExportDecl::All {
        // `export * as ns from "m"` names the re-export; plain `export * from
        // "m"` has no alias and no local binding at all.
        alias: e.exported.as_ref().map(lower_module_export_name),
        source: str_lit(&e.source),
        attributes: lower_with_clause(e.with_clause.as_deref()),
    })
}
