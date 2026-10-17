//! Laying a parsed [`Module`] out as a [`tree`](super) in a bump arena.

use super::*;

/// The module's statements as a tree in `bump`, or the one error CPython's
/// compiler reports that its parser does not and ZIPP's compiler would not:
/// a type parameter without a default after one with a default.
pub fn build<'a>(module: &Module, bump: &'a Bump) -> Result<Seq<'a, Stmt<'a>>, Error> {
    let mut b = Builder {
        m: module,
        bump,
        names: vec![None; module.names.len()],
        work: Vec::new(),
        values: Vec::new(),
        children: Vec::new(),
    };
    b.stmts(module.body)
}

#[inline]
fn tr(r: a::Range) -> TextRange {
    TextRange::new(r.start, r.end)
}

fn ctx(c: a::ExprContext) -> ExprContext {
    match c {
        a::ExprContext::Load => ExprContext::Load,
        a::ExprContext::Store => ExprContext::Store,
        a::ExprContext::Del => ExprContext::Del,
    }
}

fn operator(op: a::Operator) -> Operator {
    use a::Operator as O;
    match op {
        O::Add => Operator::Add,
        O::Sub => Operator::Sub,
        O::Mult => Operator::Mult,
        O::MatMult => Operator::MatMult,
        O::Div => Operator::Div,
        O::Mod => Operator::Mod,
        O::Pow => Operator::Pow,
        O::LShift => Operator::LShift,
        O::RShift => Operator::RShift,
        O::BitOr => Operator::BitOr,
        O::BitXor => Operator::BitXor,
        O::BitAnd => Operator::BitAnd,
        O::FloorDiv => Operator::FloorDiv,
    }
}

fn cmp_op(op: a::CmpOp) -> CmpOp {
    use a::CmpOp as C;
    match op {
        C::Eq => CmpOp::Eq,
        C::NotEq => CmpOp::NotEq,
        C::Lt => CmpOp::Lt,
        C::LtE => CmpOp::LtE,
        C::Gt => CmpOp::Gt,
        C::GtE => CmpOp::GtE,
        C::Is => CmpOp::Is,
        C::IsNot => CmpOp::IsNot,
        C::In => CmpOp::In,
        C::NotIn => CmpOp::NotIn,
    }
}

enum Work {
    Visit(ExprId),
    Build(ExprId),
}

struct Builder<'m, 's, 'a> {
    m: &'m Module<'s>,
    bump: &'a Bump,
    /// Each interned name, copied into the arena once.
    names: Vec<Option<&'a str>>,
    work: Vec<Work>,
    values: Vec<Expr<'a>>,
    children: Vec<ExprId>,
}

impl<'a> Builder<'_, '_, 'a> {
    fn id(&mut self, sym: crate::intern::Sym) -> Identifier<'a> {
        let i = sym.0 as usize;
        if let Some(name) = self.names[i] {
            return Identifier(name);
        }
        let name: &'a str = self.bump.alloc_str(self.m.name(sym));
        self.names[i] = Some(name);
        Identifier(name)
    }

    fn seq<T>(&self, items: impl ExactSizeIterator<Item = T>) -> Seq<'a, T> {
        if items.len() == 0 {
            return Seq(&[]);
        }
        Seq(self.bump.alloc_slice_fill_iter(items))
    }

    fn boxed(&self, e: Expr<'a>) -> &'a Expr<'a> {
        self.bump.alloc(e)
    }

    fn constant(&self, value: a::Constant) -> (Constant<'a>, Option<&'a str>) {
        let m = self.m;
        match value {
            a::Constant::None => (Constant::None, None),
            a::Constant::True => (Constant::Bool(true), None),
            a::Constant::False => (Constant::Bool(false), None),
            a::Constant::Ellipsis => (Constant::Ellipsis, None),
            a::Constant::Int { radix, text } => (
                Constant::Int(IntLit {
                    text: self.bump.alloc_str(m.number_text(text)),
                    radix,
                }),
                None,
            ),
            a::Constant::Float { text } => (Constant::Float(m.float_value(text)), None),
            a::Constant::Complex { text } => (
                Constant::Complex {
                    real: 0.0,
                    imag: m.complex_value(text),
                },
                None,
            ),
            a::Constant::Str { value, u } => (
                Constant::Str(Str(self.bump.alloc_str(m.str(value)))),
                u.then_some("u"),
            ),
            a::Constant::Bytes { value } => (
                Constant::Bytes(self.bump.alloc_slice_copy(m.bytes(value))),
                None,
            ),
        }
    }

    /// Build an expression (iteratively).
    fn expr(&mut self, root: ExprId) -> Expr<'a> {
        let base = self.values.len();
        self.work.push(Work::Visit(root));
        while let Some(work) = self.work.pop() {
            match work {
                Work::Visit(id) => {
                    self.work.push(Work::Build(id));
                    let start = self.children.len();
                    self.push_children(id);
                    // Visit in reverse so the results land in order.
                    for i in (start..self.children.len()).rev() {
                        let child = self.children[i];
                        self.work.push(Work::Visit(child));
                    }
                    self.children.truncate(start);
                }
                Work::Build(id) => {
                    let expr = self.build(id);
                    self.values.push(expr);
                }
            }
        }
        debug_assert_eq!(self.values.len(), base + 1);
        self.values.pop().unwrap()
    }

    fn boxed_expr(&mut self, id: ExprId) -> &'a Expr<'a> {
        let e = self.expr(id);
        self.boxed(e)
    }

    fn opt_expr(&mut self, id: Option<ExprId>) -> Option<&'a Expr<'a>> {
        id.map(|id| self.boxed_expr(id))
    }

    fn exprs(&mut self, list: a::List<ExprId>) -> Seq<'a, Expr<'a>> {
        let m = self.m;
        let built: Vec<Expr<'a>> = m.list(list).iter().map(|&id| self.expr(id)).collect();
        self.seq(built.into_iter())
    }

    /// An expression's children, in the order `build` takes them.
    fn push_children(&mut self, id: ExprId) {
        let m = self.m;
        let ch = &mut self.children;
        let comps = |ch: &mut Vec<ExprId>, list: a::List<a::Comprehension>| {
            for c in m.list(list) {
                ch.push(c.target);
                ch.push(c.iter);
                ch.extend_from_slice(m.list(c.ifs));
            }
        };
        match m.expr(id).kind {
            ExprKind::BoolOp { values, .. } => ch.extend_from_slice(m.list(values)),
            ExprKind::NamedExpr { target, value } => ch.extend([target, value]),
            ExprKind::BinOp { left, right, .. } => ch.extend([left, right]),
            ExprKind::UnaryOp { operand, .. } => ch.push(operand),
            ExprKind::Lambda { args, body } => {
                let args = m.arguments(args);
                for p in m
                    .list(args.posonlyargs)
                    .iter()
                    .chain(m.list(args.args))
                    .chain(m.list(args.kwonlyargs))
                {
                    ch.extend(p.default);
                }
                ch.push(body);
            }
            ExprKind::IfExp { test, body, orelse } => ch.extend([test, body, orelse]),
            ExprKind::Dict { keys, values } => {
                for (k, &v) in m.list(keys).iter().zip(m.list(values)) {
                    ch.extend(*k);
                    ch.push(v);
                }
            }
            ExprKind::Set { elts } => ch.extend_from_slice(m.list(elts)),
            ExprKind::ListComp { elt, generators }
            | ExprKind::SetComp { elt, generators }
            | ExprKind::GeneratorExp { elt, generators } => {
                ch.push(elt);
                comps(ch, generators);
            }
            ExprKind::DictComp {
                key,
                value,
                generators,
            } => {
                ch.extend([key, value]);
                comps(ch, generators);
            }
            ExprKind::Await { value } | ExprKind::YieldFrom { value } => ch.push(value),
            ExprKind::Yield { value } => ch.extend(value),
            ExprKind::Compare {
                left, comparators, ..
            } => {
                ch.push(left);
                ch.extend_from_slice(m.list(comparators));
            }
            ExprKind::Call {
                func,
                args,
                keywords,
            } => {
                ch.push(func);
                ch.extend_from_slice(m.list(args));
                ch.extend(m.list(keywords).iter().map(|k| k.value));
            }
            ExprKind::FormattedValue {
                value, format_spec, ..
            } => {
                ch.push(value);
                ch.extend(format_spec);
            }
            ExprKind::JoinedStr { values } => ch.extend_from_slice(m.list(values)),
            ExprKind::Constant(_) | ExprKind::Name { .. } => {}
            ExprKind::Attribute { value, .. } | ExprKind::Starred { value, .. } => ch.push(value),
            ExprKind::Subscript { value, slice, .. } => ch.extend([value, slice]),
            ExprKind::List { elts, .. } | ExprKind::Tuple { elts, .. } => {
                ch.extend_from_slice(m.list(elts))
            }
            ExprKind::Slice { lower, upper, step } => {
                ch.extend(lower);
                ch.extend(upper);
                ch.extend(step);
            }
        }
    }

    fn build(&mut self, id: ExprId) -> Expr<'a> {
        let m = self.m;
        let expr = m.expr(id);
        let range = tr(expr.range);
        match expr.kind {
            ExprKind::Constant(value) => {
                let (value, kind) = self.constant(value);
                return ExprConstant { value, kind, range }.into();
            }
            ExprKind::Name { id: name, ctx: c } => {
                return ExprName {
                    id: self.id(name),
                    ctx: ctx(c),
                    range,
                }
                .into();
            }
            ExprKind::Attribute { attr, .. } => {
                // Interned before the children are taken below.
                self.id(attr);
            }
            ExprKind::Call { keywords, .. } => {
                for k in m.list(keywords) {
                    if let Some(s) = k.arg {
                        self.id(s);
                    }
                }
            }
            ExprKind::Lambda { args, .. } => {
                let args = *m.arguments(args);
                for p in m
                    .list(args.posonlyargs)
                    .iter()
                    .chain(m.list(args.args))
                    .chain(m.list(args.kwonlyargs))
                    .chain(args.vararg.map(|i| &m.params[i as usize]))
                    .chain(args.kwarg.map(|i| &m.params[i as usize]))
                {
                    self.id(p.name);
                }
            }
            _ => {}
        }
        // The children, built, in `push_children` order.
        let count = {
            let start = self.children.len();
            self.push_children(id);
            let n = self.children.len() - start;
            self.children.truncate(start);
            n
        };
        let at = self.values.len() - count;
        let bump = self.bump;
        let names = &self.names;
        let name = |s: crate::intern::Sym| Identifier(names[s.0 as usize].unwrap());
        let mut kids = self.values.drain(at..);
        let mut next = || kids.next().unwrap();
        let seq = |n: usize, next: &mut dyn FnMut() -> Expr<'a>| -> Seq<'a, Expr<'a>> {
            if n == 0 {
                Seq(&[])
            } else {
                Seq(bump.alloc_slice_fill_with(n, |_| next()))
            }
        };
        let boxed = |e: Expr<'a>| -> &'a Expr<'a> { bump.alloc(e) };
        let comprehensions = |next: &mut dyn FnMut() -> Expr<'a>,
                              list: a::List<a::Comprehension>|
         -> Seq<'a, Comprehension<'a>> {
            let list = m.list(list);
            if list.is_empty() {
                return Seq(&[]);
            }
            Seq(
                bump.alloc_slice_fill_iter(list.iter().map(|c| Comprehension {
                    target: next(),
                    iter: next(),
                    ifs: seq(c.ifs.len(), next),
                    is_async: c.is_async,
                    range: tr(c.range),
                })),
            )
        };
        match expr.kind {
            ExprKind::BoolOp { op, values } => ExprBoolOp {
                op: match op {
                    a::BoolOp::And => BoolOp::And,
                    a::BoolOp::Or => BoolOp::Or,
                },
                values: seq(values.len(), &mut next),
                range,
            }
            .into(),
            ExprKind::NamedExpr { .. } => ExprNamedExpr {
                target: boxed(next()),
                value: boxed(next()),
                range,
            }
            .into(),
            ExprKind::BinOp { op, .. } => ExprBinOp {
                left: boxed(next()),
                op: operator(op),
                right: boxed(next()),
                range,
            }
            .into(),
            ExprKind::UnaryOp { op, .. } => ExprUnaryOp {
                op: match op {
                    a::UnaryOp::Invert => UnaryOp::Invert,
                    a::UnaryOp::Not => UnaryOp::Not,
                    a::UnaryOp::UAdd => UnaryOp::UAdd,
                    a::UnaryOp::USub => UnaryOp::USub,
                },
                operand: boxed(next()),
                range,
            }
            .into(),
            ExprKind::Lambda { args, .. } => {
                let args = lambda_arguments(m, bump, &name, args, &mut next);
                ExprLambda {
                    args: bump.alloc(args),
                    body: boxed(next()),
                    range,
                }
                .into()
            }
            ExprKind::IfExp { .. } => ExprIfExp {
                test: boxed(next()),
                body: boxed(next()),
                orelse: boxed(next()),
                range,
            }
            .into(),
            ExprKind::Dict { keys, values } => {
                let n = values.len();
                let mut ks = Vec::with_capacity(n);
                let mut vs = Vec::with_capacity(n);
                for k in m.list(keys) {
                    ks.push(k.map(|_| next()));
                    vs.push(next());
                }
                let keys = if n == 0 {
                    Seq(&[])
                } else {
                    Seq(&*bump.alloc_slice_fill_iter(ks))
                };
                let values = if n == 0 {
                    Seq(&[])
                } else {
                    Seq(&*bump.alloc_slice_fill_iter(vs))
                };
                ExprDict {
                    keys,
                    values,
                    range,
                }
                .into()
            }
            ExprKind::Set { elts } => ExprSet {
                elts: seq(elts.len(), &mut next),
                range,
            }
            .into(),
            ExprKind::ListComp { generators, .. } => {
                let elt = boxed(next());
                ExprListComp {
                    elt,
                    generators: comprehensions(&mut next, generators),
                    range,
                }
                .into()
            }
            ExprKind::SetComp { generators, .. } => {
                let elt = boxed(next());
                ExprSetComp {
                    elt,
                    generators: comprehensions(&mut next, generators),
                    range,
                }
                .into()
            }
            ExprKind::GeneratorExp { generators, .. } => {
                let elt = boxed(next());
                ExprGeneratorExp {
                    elt,
                    generators: comprehensions(&mut next, generators),
                    range,
                }
                .into()
            }
            ExprKind::DictComp { generators, .. } => {
                let key = boxed(next());
                let value = boxed(next());
                ExprDictComp {
                    key,
                    value,
                    generators: comprehensions(&mut next, generators),
                    range,
                }
                .into()
            }
            ExprKind::Await { .. } => ExprAwait {
                value: boxed(next()),
                range,
            }
            .into(),
            ExprKind::Yield { value } => ExprYield {
                value: value.map(|_| boxed(next())),
                range,
            }
            .into(),
            ExprKind::YieldFrom { .. } => ExprYieldFrom {
                value: boxed(next()),
                range,
            }
            .into(),
            ExprKind::Compare {
                ops, comparators, ..
            } => {
                let left = boxed(next());
                let ops = m.list(ops);
                ExprCompare {
                    left,
                    ops: Seq(bump.alloc_slice_fill_iter(ops.iter().map(|&op| cmp_op(op)))),
                    comparators: seq(comparators.len(), &mut next),
                    range,
                }
                .into()
            }
            ExprKind::Call { args, keywords, .. } => {
                let func = boxed(next());
                let args = seq(args.len(), &mut next);
                let keywords = m.list(keywords);
                let keywords = if keywords.is_empty() {
                    Seq(&[])
                } else {
                    Seq(
                        &*bump.alloc_slice_fill_iter(keywords.iter().map(|k| Keyword {
                            arg: k.arg.map(name),
                            value: next(),
                            range: tr(k.range),
                        })),
                    )
                };
                ExprCall {
                    func,
                    args,
                    keywords,
                    range,
                }
                .into()
            }
            ExprKind::FormattedValue {
                conversion,
                format_spec,
                ..
            } => ExprFormattedValue {
                value: boxed(next()),
                conversion: match conversion {
                    a::Conversion::None => ConversionFlag::None,
                    a::Conversion::Str => ConversionFlag::Str,
                    a::Conversion::Repr => ConversionFlag::Repr,
                    a::Conversion::Ascii => ConversionFlag::Ascii,
                },
                format_spec: format_spec.map(|_| boxed(next())),
                range,
            }
            .into(),
            ExprKind::JoinedStr { values } => ExprJoinedStr {
                values: seq(values.len(), &mut next),
                range,
            }
            .into(),
            ExprKind::Constant(_) | ExprKind::Name { .. } => unreachable!(),
            ExprKind::Attribute { attr, ctx: c, .. } => ExprAttribute {
                value: boxed(next()),
                attr: name(attr),
                ctx: ctx(c),
                range,
            }
            .into(),
            ExprKind::Subscript { ctx: c, .. } => ExprSubscript {
                value: boxed(next()),
                slice: boxed(next()),
                ctx: ctx(c),
                range,
            }
            .into(),
            ExprKind::Starred { ctx: c, .. } => ExprStarred {
                value: boxed(next()),
                ctx: ctx(c),
                range,
            }
            .into(),
            ExprKind::List { elts, ctx: c } => ExprList {
                elts: seq(elts.len(), &mut next),
                ctx: ctx(c),
                range,
            }
            .into(),
            ExprKind::Tuple { elts, ctx: c } => ExprTuple {
                elts: seq(elts.len(), &mut next),
                ctx: ctx(c),
                range,
            }
            .into(),
            ExprKind::Slice { lower, upper, step } => ExprSlice {
                lower: lower.map(|_| boxed(next())),
                upper: upper.map(|_| boxed(next())),
                step: step.map(|_| boxed(next())),
                range,
            }
            .into(),
        }
    }

    fn stmts(&mut self, list: a::List<a::StmtId>) -> Result<Seq<'a, Stmt<'a>>, Error> {
        let m = self.m;
        let mut built = Vec::with_capacity(list.len());
        for &id in m.list(list) {
            built.push(self.stmt(id)?);
        }
        Ok(self.seq(built.into_iter()))
    }

    fn arguments(&mut self, id: a::ArgsId) -> &'a Arguments<'a> {
        let m = self.m;
        let args = *m.arguments(id);
        let posonlyargs = self.params_with_default(args.posonlyargs);
        let argsv = self.params_with_default(args.args);
        let vararg = args.vararg.map(|i| {
            let p = self.param(&m.params[i as usize]);
            &*self.bump.alloc(p)
        });
        let kwonlyargs = self.params_with_default(args.kwonlyargs);
        let kwarg = args.kwarg.map(|i| {
            let p = self.param(&m.params[i as usize]);
            &*self.bump.alloc(p)
        });
        self.bump.alloc(Arguments {
            posonlyargs,
            args: argsv,
            vararg,
            kwonlyargs,
            kwarg,
            range: tr(args.range),
        })
    }

    fn param(&mut self, p: &a::Param) -> Arg<'a> {
        Arg {
            arg: self.id(p.name),
            annotation: self.opt_expr(p.annotation),
            range: tr(p.range),
        }
    }

    fn params_with_default(&mut self, list: a::List<a::Param>) -> Seq<'a, ArgWithDefault<'a>> {
        let m = self.m;
        let built: Vec<_> = m
            .list(list)
            .iter()
            .map(|p| ArgWithDefault {
                def: self.param(p),
                default: self.opt_expr(p.default),
                range: tr(p.range),
            })
            .collect();
        self.seq(built.into_iter())
    }

    fn type_params(
        &mut self,
        list: a::List<a::TypeParam>,
    ) -> Result<Seq<'a, TypeParam<'a>>, Error> {
        let m = self.m;
        let mut out = Vec::with_capacity(list.len());
        let mut defaults = false;
        for p in m.list(list) {
            // As CPython's compiler checks (its parser does not).
            if defaults && p.default.is_none() {
                return Err(Error::new(
                    format!(
                        "non-default type parameter '{}' follows default type parameter",
                        m.name(p.name)
                    ),
                    p.range.start,
                ));
            }
            defaults |= p.default.is_some();
            let name = self.id(p.name);
            let range = tr(p.range);
            out.push(match p.kind {
                a::TypeParamKind::TypeVar { bound } => {
                    let bound = self.opt_expr(bound);
                    TypeParamTypeVar {
                        name,
                        bound,
                        default: self.opt_expr(p.default),
                        range,
                    }
                    .into()
                }
                a::TypeParamKind::ParamSpec => TypeParamParamSpec {
                    name,
                    default: self.opt_expr(p.default),
                    range,
                }
                .into(),
                a::TypeParamKind::TypeVarTuple => TypeParamTypeVarTuple {
                    name,
                    default: self.opt_expr(p.default),
                    range,
                }
                .into(),
            });
        }
        Ok(self.seq(out.into_iter()))
    }

    fn keywords(&mut self, list: a::List<a::Keyword>) -> Seq<'a, Keyword<'a>> {
        let m = self.m;
        let built: Vec<_> = m
            .list(list)
            .iter()
            .map(|k| Keyword {
                arg: k.arg.map(|s| self.id(s)),
                value: self.expr(k.value),
                range: tr(k.range),
            })
            .collect();
        self.seq(built.into_iter())
    }

    fn stmt(&mut self, id: a::StmtId) -> Result<Stmt<'a>, Error> {
        let m = self.m;
        let stmt = m.stmt(id);
        let range = tr(stmt.range);
        Ok(match stmt.kind {
            StmtKind::FunctionDef {
                is_async,
                name,
                args,
                body,
                decorator_list,
                returns,
                type_params,
            } => {
                let decorator_list = self.exprs(decorator_list);
                let type_params = self.type_params(type_params)?;
                let args = self.arguments(args);
                let returns = self.opt_expr(returns);
                let body = self.stmts(body)?;
                let name = self.id(name);
                if is_async {
                    StmtAsyncFunctionDef {
                        name,
                        args,
                        body,
                        decorator_list,
                        returns,
                        type_params,
                        range,
                    }
                    .into()
                } else {
                    StmtFunctionDef {
                        name,
                        args,
                        body,
                        decorator_list,
                        returns,
                        type_params,
                        range,
                    }
                    .into()
                }
            }
            StmtKind::ClassDef {
                name,
                bases,
                keywords,
                body,
                decorator_list,
                type_params,
            } => {
                let decorator_list = self.exprs(decorator_list);
                let type_params = self.type_params(type_params)?;
                let bases = self.exprs(bases);
                let keywords = self.keywords(keywords);
                StmtClassDef {
                    name: self.id(name),
                    bases,
                    keywords,
                    body: self.stmts(body)?,
                    decorator_list,
                    type_params,
                    range,
                }
                .into()
            }
            StmtKind::Return { value } => StmtReturn {
                value: self.opt_expr(value),
                range,
            }
            .into(),
            StmtKind::Delete { targets } => StmtDelete {
                targets: self.exprs(targets),
                range,
            }
            .into(),
            StmtKind::Assign { targets, value } => StmtAssign {
                targets: self.exprs(targets),
                value: self.boxed_expr(value),
                range,
            }
            .into(),
            StmtKind::TypeAlias {
                name,
                type_params,
                value,
            } => {
                let name = self.boxed_expr(name);
                let type_params = self.type_params(type_params)?;
                StmtTypeAlias {
                    name,
                    type_params,
                    value: self.boxed_expr(value),
                    range,
                }
                .into()
            }
            StmtKind::AugAssign { target, op, value } => StmtAugAssign {
                target: self.boxed_expr(target),
                op: operator(op),
                value: self.boxed_expr(value),
                range,
            }
            .into(),
            StmtKind::AnnAssign {
                target,
                annotation,
                value,
                simple,
            } => StmtAnnAssign {
                target: self.boxed_expr(target),
                annotation: self.boxed_expr(annotation),
                value: self.opt_expr(value),
                simple,
                range,
            }
            .into(),
            StmtKind::For {
                is_async,
                target,
                iter,
                body,
                orelse,
            } => {
                let target = self.boxed_expr(target);
                let iter = self.boxed_expr(iter);
                let body = self.stmts(body)?;
                let orelse = self.stmts(orelse)?;
                if is_async {
                    StmtAsyncFor {
                        target,
                        iter,
                        body,
                        orelse,
                        range,
                    }
                    .into()
                } else {
                    StmtFor {
                        target,
                        iter,
                        body,
                        orelse,
                        range,
                    }
                    .into()
                }
            }
            StmtKind::While { test, body, orelse } => StmtWhile {
                test: self.boxed_expr(test),
                body: self.stmts(body)?,
                orelse: self.stmts(orelse)?,
                range,
            }
            .into(),
            StmtKind::If { test, body, orelse } => StmtIf {
                test: self.boxed_expr(test),
                body: self.stmts(body)?,
                orelse: self.stmts(orelse)?,
                range,
            }
            .into(),
            StmtKind::With {
                is_async,
                items,
                body,
            } => {
                let built: Vec<_> = m
                    .list(items)
                    .iter()
                    .map(|i| WithItem {
                        context_expr: self.expr(i.context_expr),
                        optional_vars: self.opt_expr(i.optional_vars),
                        range: tr(i.range),
                    })
                    .collect();
                let items = self.seq(built.into_iter());
                let body = self.stmts(body)?;
                if is_async {
                    StmtAsyncWith { items, body, range }.into()
                } else {
                    StmtWith { items, body, range }.into()
                }
            }
            StmtKind::Match { subject, cases } => {
                let subject = self.boxed_expr(subject);
                let mut out = Vec::with_capacity(cases.len());
                for case in m.list(cases) {
                    out.push(MatchCase {
                        pattern: self.pattern(case.pattern),
                        guard: self.opt_expr(case.guard),
                        body: self.stmts(case.body)?,
                        range: tr(case.range),
                    });
                }
                StmtMatch {
                    subject,
                    cases: self.seq(out.into_iter()),
                    range,
                }
                .into()
            }
            StmtKind::Raise { exc, cause } => StmtRaise {
                exc: self.opt_expr(exc),
                cause: self.opt_expr(cause),
                range,
            }
            .into(),
            StmtKind::Try {
                star,
                body,
                handlers,
                orelse,
                finalbody,
            } => {
                let body = self.stmts(body)?;
                let mut hs = Vec::with_capacity(handlers.len());
                for h in m.list(handlers) {
                    hs.push(ExceptHandler::ExceptHandler(ExceptHandlerExceptHandler {
                        type_: self.opt_expr(h.type_),
                        name: h.name.map(|s| self.id(s)),
                        body: self.stmts(h.body)?,
                        range: tr(h.range),
                    }));
                }
                let handlers = self.seq(hs.into_iter());
                let orelse = self.stmts(orelse)?;
                let finalbody = self.stmts(finalbody)?;
                if star {
                    StmtTryStar {
                        body,
                        handlers,
                        orelse,
                        finalbody,
                        range,
                    }
                    .into()
                } else {
                    StmtTry {
                        body,
                        handlers,
                        orelse,
                        finalbody,
                        range,
                    }
                    .into()
                }
            }
            StmtKind::Assert { test, msg } => StmtAssert {
                test: self.boxed_expr(test),
                msg: self.opt_expr(msg),
                range,
            }
            .into(),
            StmtKind::Import { names } => StmtImport {
                names: self.aliases(names),
                range,
            }
            .into(),
            StmtKind::ImportFrom {
                module,
                names,
                level,
            } => StmtImportFrom {
                module: module.map(|s| self.id(s)),
                names: self.aliases(names),
                level: Some(Int::new(level)),
                range,
            }
            .into(),
            StmtKind::Global { names } => StmtGlobal {
                names: self.idents(names),
                range,
            }
            .into(),
            StmtKind::Nonlocal { names } => StmtNonlocal {
                names: self.idents(names),
                range,
            }
            .into(),
            StmtKind::Expr { value } => StmtExpr {
                value: self.boxed_expr(value),
                range,
            }
            .into(),
            StmtKind::Pass => StmtPass {
                _p: std::marker::PhantomData,
                range,
            }
            .into(),
            StmtKind::Break => StmtBreak {
                _p: std::marker::PhantomData,
                range,
            }
            .into(),
            StmtKind::Continue => StmtContinue {
                _p: std::marker::PhantomData,
                range,
            }
            .into(),
        })
    }

    fn idents(&mut self, list: a::List<crate::intern::Sym>) -> Seq<'a, Identifier<'a>> {
        let m = self.m;
        let built: Vec<_> = m.list(list).iter().map(|&s| self.id(s)).collect();
        self.seq(built.into_iter())
    }

    fn aliases(&mut self, list: a::List<a::Alias>) -> Seq<'a, Alias<'a>> {
        let m = self.m;
        let built: Vec<_> = m
            .list(list)
            .iter()
            .map(|alias| Alias {
                name: self.id(alias.name),
                asname: alias.asname.map(|s| self.id(s)),
                range: tr(alias.range),
            })
            .collect();
        self.seq(built.into_iter())
    }

    fn patterns(&mut self, list: a::List<a::PatId>) -> Seq<'a, Pattern<'a>> {
        let m = self.m;
        let built: Vec<_> = m.list(list).iter().map(|&p| self.pattern(p)).collect();
        self.seq(built.into_iter())
    }

    fn pattern(&mut self, id: a::PatId) -> Pattern<'a> {
        let m = self.m;
        let pattern = m.pattern(id);
        let range = tr(pattern.range);
        match pattern.kind {
            PatternKind::MatchValue { value } => PatternMatchValue {
                value: self.boxed_expr(value),
                range,
            }
            .into(),
            PatternKind::MatchSingleton { value } => PatternMatchSingleton {
                value: match value {
                    a::Constant::True => Constant::Bool(true),
                    a::Constant::False => Constant::Bool(false),
                    _ => Constant::None,
                },
                range,
            }
            .into(),
            PatternKind::MatchSequence { patterns } => PatternMatchSequence {
                patterns: self.patterns(patterns),
                range,
            }
            .into(),
            PatternKind::MatchMapping {
                keys,
                patterns,
                rest,
            } => PatternMatchMapping {
                keys: self.exprs(keys),
                patterns: self.patterns(patterns),
                rest: rest.map(|s| self.id(s)),
                range,
            }
            .into(),
            PatternKind::MatchClass {
                cls,
                patterns,
                kwd_attrs,
                kwd_patterns,
            } => PatternMatchClass {
                cls: self.boxed_expr(cls),
                patterns: self.patterns(patterns),
                kwd_attrs: self.idents(kwd_attrs),
                kwd_patterns: self.patterns(kwd_patterns),
                range,
            }
            .into(),
            PatternKind::MatchStar { name } => PatternMatchStar {
                name: name.map(|s| self.id(s)),
                range,
            }
            .into(),
            PatternKind::MatchAs { pattern, name } => PatternMatchAs {
                pattern: pattern.map(|p| {
                    let p = self.pattern(p);
                    &*self.bump.alloc(p)
                }),
                name: name.map(|s| self.id(s)),
                range,
            }
            .into(),
            PatternKind::MatchOr { patterns } => PatternMatchOr {
                patterns: self.patterns(patterns),
                range,
            }
            .into(),
        }
    }
}

/// A lambda's parameters: defaults come from the built children (lambda
/// parameters have no annotations; their names are interned already).
fn lambda_arguments<'a>(
    m: &Module,
    bump: &'a Bump,
    name: &dyn Fn(crate::intern::Sym) -> Identifier<'a>,
    id: a::ArgsId,
    next: &mut dyn FnMut() -> Expr<'a>,
) -> Arguments<'a> {
    let args = m.arguments(id);
    let arg = |p: &a::Param| Arg {
        arg: name(p.name),
        annotation: None,
        range: tr(p.range),
    };
    let mut with_default = |list: a::List<a::Param>| -> Seq<'a, ArgWithDefault<'a>> {
        let list = m.list(list);
        if list.is_empty() {
            return Seq(&[]);
        }
        Seq(
            bump.alloc_slice_fill_iter(list.iter().map(|p| ArgWithDefault {
                def: arg(p),
                default: p.default.map(|_| &*bump.alloc(next())),
                range: tr(p.range),
            })),
        )
    };
    let posonlyargs = with_default(args.posonlyargs);
    let argsv = with_default(args.args);
    let kwonlyargs = with_default(args.kwonlyargs);
    Arguments {
        posonlyargs,
        args: argsv,
        vararg: args
            .vararg
            .map(|i| &*bump.alloc(arg(&m.params[i as usize]))),
        kwonlyargs,
        kwarg: args.kwarg.map(|i| &*bump.alloc(arg(&m.params[i as usize]))),
        range: tr(args.range),
    }
}
