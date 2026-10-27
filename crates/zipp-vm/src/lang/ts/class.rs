//! Classes: member modifiers, index signatures, `implements`, and the
//! members that have no run-time meaning.
//!
//! A member is read by the JavaScript class parser once TypeScript's
//! modifiers (`public`, `private`, `protected`, `readonly`, `override`,
//! `abstract`, `declare`, and `static` in any position among them) are past.
//! `declare` and `abstract` members, overload signatures and index
//! signatures are dropped: tsc emits nothing for them, and a `declare`d
//! field must not define the property (it would shadow the base class's).

use crate::parse::ast::*;
use crate::parse::parser::{PResult, Parser};
use crate::parse::token::{Punct, Span};

impl<'s> Parser<'s> {
    /// A class member, or `None` for one that is only a type. A constructor's
    /// parameter properties add their fields to `fields`, which come first in
    /// the class (tsc's `useDefineForClassFields` output declares them so).
    pub(crate) fn ts_class_member(
        &mut self,
        derived: bool,
        fields: &mut Vec<ClassMember>,
    ) -> PResult<Option<ClassMember>> {
        let dec_start = self.cur().span.start;
        let mut decorators = self.parse_decorators()?;
        let mut erase = false;
        while let Some(word) = self.ts_word() {
            let is_static = word == "static";
            if !is_static
                && !matches!(
                    word,
                    "public"
                        | "private"
                        | "protected"
                        | "readonly"
                        | "override"
                        | "abstract"
                        | "declare"
                )
            {
                break;
            }
            let erases = matches!(word, "abstract" | "declare");
            // `static {` is a static block, and a word followed by what cannot
            // follow a modifier is the member's own name (`readonly: boolean`,
            // `private() {}`).
            let modifier = self
                .ts_peek(|p| p.ts_can_follow_modifier() && !(is_static && p.at(Punct::LBrace)))?;
            if !modifier {
                break;
            }
            erase |= erases;
            self.bump(false)?;
            if is_static {
                self.ts_state.member_static = true;
            }
        }
        if self.at(Punct::LBracket) && self.ts_at_index_signature()? {
            self.ts_state.member_static = false;
            self.ts_index_signature()?;
            self.semicolon()?;
            return Ok(None);
        }
        let start = self.cur().span.start;
        let mut m = self.parse_class_member_inner(derived, start)?;
        self.ts_state.member_static = false;
        let props = std::mem::take(&mut self.ts_state.ctor_props);
        if matches!(&m, ClassMember::Method(cm) if cm.kind == MethodKind::Constructor) {
            fields.extend(props.into_iter().map(|name| {
                ClassMember::Field(ClassField {
                    key: PropKey::Ident(name),
                    value: None,
                    is_static: false,
                    accessor: None,
                    decorators: Vec::new(),
                })
            }));
        }
        if std::mem::take(&mut self.ts_state.bodiless) || erase {
            if self.ts_state.legacy_decorators {
                self.ts_state.fn_param_decs.clear();
                self.ts_state.fn_sig = None;
                self.ts_state.field_type = None;
            }
            return Ok(None);
        }
        if self.ts_state.legacy_decorators {
            self.ts_legacy_member(&m, &mut decorators);
        }
        self.attach_member_decorators(&mut m, decorators, dec_start)?;
        Ok(Some(m))
    }

    /// After a class member's name: `?` (optional) or `!` (definitely
    /// assigned), a method's type parameters, a field's type.
    pub(crate) fn ts_member_key_suffix(&mut self) -> PResult<()> {
        if self.at(Punct::Question) || (self.at(Punct::Bang) && !self.cur().newline_before) {
            self.bump(false)?;
        }
        if self.at(Punct::Lt) {
            self.ts_type_params()?;
        } else if self.ts_state.decorator_metadata {
            if let Some((s, e)) = self.ts_annotation_span()? {
                self.ts_state.field_type = Some(self.ts_classify(s, e));
            }
        } else {
            self.ts_annotation_opt()?;
        }
        Ok(())
    }

    /// Between a class's heritage and its body: the type arguments of
    /// `extends Base<T>` and the `implements` list.
    pub(crate) fn ts_class_heritage_tail(&mut self) -> PResult<()> {
        if self.at(Punct::Lt) {
            self.ts_type_args()?;
        }
        if self.ts_at("implements") {
            self.bump(false)?;
            loop {
                self.ts_type()?;
                if !self.eat(Punct::Comma, false)? {
                    break;
                }
            }
        }
        Ok(())
    }

    /// Settle a class's fields once its body is read. With
    /// `useDefineForClassFields` they stay fields, and the parameter
    /// properties' own (`prop_fields`) go first, as tsc declares them.
    /// Without it they are tsc's assignments: an instance field with an
    /// initializer becomes `this.x = init` at the head of the constructor
    /// (after its `super(…)` and its parameter properties, in that order; a
    /// constructor is made for a class without one), and one without is
    /// dropped. Private, computed, decorated and `accessor` fields, and
    /// static ones, stay fields under either.
    pub(crate) fn ts_finish_class(
        &mut self,
        body: &mut Vec<ClassMember>,
        prop_fields: Vec<ClassMember>,
        derived: bool,
        start: u32,
    ) {
        if self.ts_state.define_fields {
            if !prop_fields.is_empty() {
                body.splice(0..0, prop_fields);
            }
            return;
        }
        let assigned = |f: &ClassField| {
            !f.is_static
                && f.accessor.is_none()
                && f.decorators.is_empty()
                && matches!(f.key, PropKey::Ident(_) | PropKey::Str(_) | PropKey::Num(_))
        };
        if !body
            .iter()
            .any(|m| matches!(m, ClassMember::Field(f) if assigned(f)))
        {
            return;
        }
        let mut inits = Vec::new();
        let mut kept = Vec::with_capacity(body.len());
        for m in body.drain(..) {
            match m {
                ClassMember::Field(f) if assigned(&f) => {
                    if let Some(value) = f.value {
                        inits.push(this_assign(f.key, value));
                    }
                }
                other => kept.push(other),
            }
        }
        *body = kept;
        if inits.is_empty() {
            return;
        }
        let ctor = body.iter_mut().find_map(|m| match m {
            ClassMember::Method(cm) if cm.kind == MethodKind::Constructor => Some(cm),
            _ => None,
        });
        match ctor {
            Some(cm) => {
                let stmts = &mut cm.func.body.stmts;
                let at = super::lower::after_super(stmts) + prop_fields.len();
                stmts.splice(at..at, inits);
            }
            None => {
                // `constructor(...args) { super(...args); … }` / `constructor() { … }`.
                // The rest parameter's name cannot be spelled, so no initializer
                // can mean it; the span (one byte at the class) is no real
                // function's.
                let args: Name = "%args".into();
                let (params, mut stmts) = if derived {
                    (
                        Params {
                            items: vec![Pattern::Rest(Box::new(Pattern::Ident(args.clone())))],
                            simple: false,
                        },
                        vec![Stmt::Expr(Expr::Call(Box::new(CallExpr {
                            callee: Expr::Super,
                            args: vec![Arg::Spread(Expr::Ident(args))],
                            optional: false,
                            pos: start,
                        })))],
                    )
                } else {
                    (
                        Params {
                            items: Vec::new(),
                            simple: true,
                        },
                        Vec::new(),
                    )
                };
                stmts.extend(inits);
                body.insert(
                    0,
                    ClassMember::Method(ClassMethod {
                        key: PropKey::Ident("constructor".into()),
                        kind: MethodKind::Constructor,
                        func: Box::new(Function {
                            name: None,
                            params,
                            body: FnBody {
                                directives: Vec::new(),
                                stmts,
                            },
                            is_async: false,
                            is_generator: false,
                            span: Span::new(start, start + 1),
                        }),
                        is_static: false,
                        decorators: Vec::new(),
                    }),
                );
            }
        }
    }
}

/// `this.key = value;` for a field's key.
fn this_assign(key: PropKey, value: Expr) -> Stmt {
    let prop = match key {
        PropKey::Ident(n) => MemberProp::Ident(n),
        PropKey::Str(s) => MemberProp::Computed(Box::new(Expr::Str(s))),
        PropKey::Num(n) => MemberProp::Computed(Box::new(Expr::Num(n))),
        PropKey::Computed(_) | PropKey::Private(_) => {
            unreachable!("only named fields are assigned")
        }
    };
    Stmt::Expr(Expr::Assign {
        op: AssignOp::Assign,
        target: Target::Member(Box::new(Member {
            object: Expr::This,
            prop,
            optional: false,
        })),
        value: Box::new(value),
        covered: false,
    })
}
