//! The arena AST as `rustpython_ast` nodes: what ZIPP's emitter consumes
//! until it reads the arena directly. For every program the RustPython 0.4
//! parser accepted, the result equals what it built, ranges included.
//!
//! Expressions are converted without recursion (an explicit stack): a
//! million-link chain (`- - - x`, `a.b.c...`, `1 + 1 + ...`) converts on a
//! small stack, as the LR parser built it, and ZIPP bounds nesting itself
//! afterwards. Statements nest only as deep as indentation allows.

use crate::ast::{self as a, ExprId, ExprKind, Module, PatternKind, StmtKind};
use crate::lexer::{Error, Limits, Mode};
use rustpython_ast as ast;
use rustpython_ast::text_size::{TextRange, TextSize};

/// Parse a module and convert it (the error when either fails). `source`
/// starts at offset 0.
pub fn parse_module(source: &str, limits: Limits) -> Result<Vec<ast::Stmt>, Error> {
    let module = crate::parse(source, Mode::Module, 0, limits)?;
    to_rustpython(&module)
}

/// Parse and convert in any mode, as `rustpython_parser::parse` does.
pub fn parse(source: &str, mode: Mode, base: u32) -> Result<ast::Mod, Error> {
    let module = crate::parse(source, mode, base, Limits::NONE)?;
    let body = to_rustpython(&module)?;
    let whole = opt(a::Range::new(0, module.end));
    Ok(match mode {
        Mode::Module => ast::ModModule {
            body,
            type_ignores: vec![],
            range: whole,
        }
        .into(),
        Mode::Interactive => ast::ModInteractive { body, range: whole }.into(),
        Mode::Expression => {
            let expr = match body.into_iter().next() {
                Some(ast::Stmt::Expr(stmt)) => stmt.value,
                _ => unreachable!("an expression parses to one expression statement"),
            };
            ast::ModExpression {
                body: expr,
                range: whole,
            }
            .into()
        }
    })
}

/// The module's statements as `rustpython_ast` nodes. Fails only on syntax
/// those nodes cannot represent (PEP 696 type parameter defaults).
pub fn to_rustpython(module: &Module) -> Result<Vec<ast::Stmt>, Error> {
    let mut c = Converter {
        m: module,
        work: Vec::new(),
        values: Vec::new(),
        children: Vec::new(),
    };
    c.stmts(module.body)
}

#[inline]
fn tr(r: a::Range) -> TextRange {
    TextRange::new(TextSize::new(r.start), TextSize::new(r.end))
}

#[inline]
fn opt(r: a::Range) -> ast::OptionalRange<TextRange> {
    tr(r).into()
}

fn ctx(c: a::ExprContext) -> ast::ExprContext {
    match c {
        a::ExprContext::Load => ast::ExprContext::Load,
        a::ExprContext::Store => ast::ExprContext::Store,
        a::ExprContext::Del => ast::ExprContext::Del,
    }
}

fn operator(op: a::Operator) -> ast::Operator {
    use a::Operator as O;
    match op {
        O::Add => ast::Operator::Add,
        O::Sub => ast::Operator::Sub,
        O::Mult => ast::Operator::Mult,
        O::MatMult => ast::Operator::MatMult,
        O::Div => ast::Operator::Div,
        O::Mod => ast::Operator::Mod,
        O::Pow => ast::Operator::Pow,
        O::LShift => ast::Operator::LShift,
        O::RShift => ast::Operator::RShift,
        O::BitOr => ast::Operator::BitOr,
        O::BitXor => ast::Operator::BitXor,
        O::BitAnd => ast::Operator::BitAnd,
        O::FloorDiv => ast::Operator::FloorDiv,
    }
}

fn cmp_op(op: a::CmpOp) -> ast::CmpOp {
    use a::CmpOp as C;
    match op {
        C::Eq => ast::CmpOp::Eq,
        C::NotEq => ast::CmpOp::NotEq,
        C::Lt => ast::CmpOp::Lt,
        C::LtE => ast::CmpOp::LtE,
        C::Gt => ast::CmpOp::Gt,
        C::GtE => ast::CmpOp::GtE,
        C::Is => ast::CmpOp::Is,
        C::IsNot => ast::CmpOp::IsNot,
        C::In => ast::CmpOp::In,
        C::NotIn => ast::CmpOp::NotIn,
    }
}

enum Work {
    Visit(ExprId),
    Build(ExprId),
}

struct Converter<'m, 's> {
    m: &'m Module<'s>,
    work: Vec<Work>,
    values: Vec<ast::Expr>,
    children: Vec<ExprId>,
}

impl Converter<'_, '_> {
    fn id(&self, sym: crate::intern::Sym) -> ast::Identifier {
        ast::Identifier::new(self.m.name(sym))
    }

    fn constant(&self, value: a::Constant) -> (ast::Constant, Option<String>) {
        let m = self.m;
        match value {
            a::Constant::None => (ast::Constant::None, None),
            a::Constant::True => (ast::Constant::Bool(true), None),
            a::Constant::False => (ast::Constant::Bool(false), None),
            a::Constant::Ellipsis => (ast::Constant::Ellipsis, None),
            a::Constant::Int { radix, text } => {
                if let Some(small) = crate::numbers::small_int(m.number_text(text), radix) {
                    return (ast::Constant::Int(small.into()), None);
                }
                let (digits, radix) = m.int_digits(radix, text);
                let value =
                    ast::bigint::BigInt::parse_bytes(digits.as_bytes(), radix).unwrap_or_default();
                (ast::Constant::Int(value), None)
            }
            a::Constant::Float { text } => (ast::Constant::Float(m.float_value(text)), None),
            a::Constant::Complex { text } => (
                ast::Constant::Complex {
                    real: 0.0,
                    imag: m.complex_value(text),
                },
                None,
            ),
            a::Constant::Str { value, u } => (
                ast::Constant::Str(m.str(value).to_owned()),
                u.then(|| "u".to_owned()),
            ),
            a::Constant::Bytes { value } => (ast::Constant::Bytes(m.bytes(value).to_vec()), None),
        }
    }

    /// Convert an expression (iteratively).
    fn expr(&mut self, root: ExprId) -> ast::Expr {
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

    fn opt_expr(&mut self, id: Option<ExprId>) -> Option<Box<ast::Expr>> {
        id.map(|id| Box::new(self.expr(id)))
    }

    fn exprs(&mut self, list: a::List<ExprId>) -> Vec<ast::Expr> {
        let m = self.m;
        m.list(list).iter().map(|&id| self.expr(id)).collect()
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

    fn build(&mut self, id: ExprId) -> ast::Expr {
        let m = self.m;
        let expr = m.expr(id);
        let range = tr(expr.range);
        if let ExprKind::Constant(value) = expr.kind {
            let (value, kind) = self.constant(value);
            return ast::ExprConstant { value, kind, range }.into();
        }
        // The children, converted, in `push_children` order.
        let count = {
            let start = self.children.len();
            self.push_children(id);
            let n = self.children.len() - start;
            self.children.truncate(start);
            n
        };
        let at = self.values.len() - count;
        let mut kids = self.values.drain(at..);
        let mut next = || kids.next().unwrap();
        let comprehensions = |next: &mut dyn FnMut() -> ast::Expr,
                              list: a::List<a::Comprehension>| {
            m.list(list)
                .iter()
                .map(|c| ast::Comprehension {
                    target: next(),
                    iter: next(),
                    ifs: (0..c.ifs.len()).map(|_| next()).collect(),
                    is_async: c.is_async,
                    range: opt(c.range),
                })
                .collect::<Vec<_>>()
        };
        let result = match expr.kind {
            ExprKind::BoolOp { op, values } => ast::ExprBoolOp {
                op: match op {
                    a::BoolOp::And => ast::BoolOp::And,
                    a::BoolOp::Or => ast::BoolOp::Or,
                },
                values: (0..values.len()).map(|_| next()).collect(),
                range,
            }
            .into(),
            ExprKind::NamedExpr { .. } => ast::ExprNamedExpr {
                target: Box::new(next()),
                value: Box::new(next()),
                range,
            }
            .into(),
            ExprKind::BinOp { op, .. } => ast::ExprBinOp {
                left: Box::new(next()),
                op: operator(op),
                right: Box::new(next()),
                range,
            }
            .into(),
            ExprKind::UnaryOp { op, .. } => ast::ExprUnaryOp {
                op: match op {
                    a::UnaryOp::Invert => ast::UnaryOp::Invert,
                    a::UnaryOp::Not => ast::UnaryOp::Not,
                    a::UnaryOp::UAdd => ast::UnaryOp::UAdd,
                    a::UnaryOp::USub => ast::UnaryOp::USub,
                },
                operand: Box::new(next()),
                range,
            }
            .into(),
            ExprKind::Lambda { args, .. } => {
                let args = lambda_arguments(m, args, &mut next);
                ast::ExprLambda {
                    args: Box::new(args),
                    body: Box::new(next()),
                    range,
                }
                .into()
            }
            ExprKind::IfExp { .. } => ast::ExprIfExp {
                test: Box::new(next()),
                body: Box::new(next()),
                orelse: Box::new(next()),
                range,
            }
            .into(),
            ExprKind::Dict { keys, values } => {
                let mut ks = Vec::with_capacity(values.len());
                let mut vs = Vec::with_capacity(values.len());
                for k in m.list(keys) {
                    ks.push(k.map(|_| next()));
                    vs.push(next());
                }
                ast::ExprDict {
                    keys: ks,
                    values: vs,
                    range,
                }
                .into()
            }
            ExprKind::Set { elts } => ast::ExprSet {
                elts: (0..elts.len()).map(|_| next()).collect(),
                range,
            }
            .into(),
            ExprKind::ListComp { generators, .. } => {
                let elt = Box::new(next());
                ast::ExprListComp {
                    elt,
                    generators: comprehensions(&mut next, generators),
                    range,
                }
                .into()
            }
            ExprKind::SetComp { generators, .. } => {
                let elt = Box::new(next());
                ast::ExprSetComp {
                    elt,
                    generators: comprehensions(&mut next, generators),
                    range,
                }
                .into()
            }
            ExprKind::GeneratorExp { generators, .. } => {
                let elt = Box::new(next());
                ast::ExprGeneratorExp {
                    elt,
                    generators: comprehensions(&mut next, generators),
                    range,
                }
                .into()
            }
            ExprKind::DictComp { generators, .. } => {
                let key = Box::new(next());
                let value = Box::new(next());
                ast::ExprDictComp {
                    key,
                    value,
                    generators: comprehensions(&mut next, generators),
                    range,
                }
                .into()
            }
            ExprKind::Await { .. } => ast::ExprAwait {
                value: Box::new(next()),
                range,
            }
            .into(),
            ExprKind::Yield { value } => ast::ExprYield {
                value: value.map(|_| Box::new(next())),
                range,
            }
            .into(),
            ExprKind::YieldFrom { .. } => ast::ExprYieldFrom {
                value: Box::new(next()),
                range,
            }
            .into(),
            ExprKind::Compare {
                ops, comparators, ..
            } => ast::ExprCompare {
                left: Box::new(next()),
                ops: m.list(ops).iter().map(|&op| cmp_op(op)).collect(),
                comparators: (0..comparators.len()).map(|_| next()).collect(),
                range,
            }
            .into(),
            ExprKind::Call { args, keywords, .. } => {
                let func = Box::new(next());
                let args = (0..args.len()).map(|_| next()).collect();
                let keywords = m
                    .list(keywords)
                    .iter()
                    .map(|k| ast::Keyword {
                        arg: k.arg.map(|s| ast::Identifier::new(m.name(s))),
                        value: next(),
                        range: tr(k.range),
                    })
                    .collect();
                ast::ExprCall {
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
            } => ast::ExprFormattedValue {
                value: Box::new(next()),
                conversion: match conversion {
                    a::Conversion::None => ast::ConversionFlag::None,
                    a::Conversion::Str => ast::ConversionFlag::Str,
                    a::Conversion::Repr => ast::ConversionFlag::Repr,
                    a::Conversion::Ascii => ast::ConversionFlag::Ascii,
                },
                format_spec: format_spec.map(|_| Box::new(next())),
                range,
            }
            .into(),
            ExprKind::JoinedStr { values } => ast::ExprJoinedStr {
                values: (0..values.len()).map(|_| next()).collect(),
                range,
            }
            .into(),
            ExprKind::Constant(_) => unreachable!(),
            ExprKind::Attribute { attr, ctx: c, .. } => ast::ExprAttribute {
                value: Box::new(next()),
                attr: ast::Identifier::new(m.name(attr)),
                ctx: ctx(c),
                range,
            }
            .into(),
            ExprKind::Subscript { ctx: c, .. } => ast::ExprSubscript {
                value: Box::new(next()),
                slice: Box::new(next()),
                ctx: ctx(c),
                range,
            }
            .into(),
            ExprKind::Starred { ctx: c, .. } => ast::ExprStarred {
                value: Box::new(next()),
                ctx: ctx(c),
                range,
            }
            .into(),
            ExprKind::Name { id, ctx: c } => ast::ExprName {
                id: ast::Identifier::new(m.name(id)),
                ctx: ctx(c),
                range,
            }
            .into(),
            ExprKind::List { elts, ctx: c } => ast::ExprList {
                elts: (0..elts.len()).map(|_| next()).collect(),
                ctx: ctx(c),
                range,
            }
            .into(),
            ExprKind::Tuple { elts, ctx: c } => ast::ExprTuple {
                elts: (0..elts.len()).map(|_| next()).collect(),
                ctx: ctx(c),
                range,
            }
            .into(),
            ExprKind::Slice { lower, upper, step } => ast::ExprSlice {
                lower: lower.map(|_| Box::new(next())),
                upper: upper.map(|_| Box::new(next())),
                step: step.map(|_| Box::new(next())),
                range,
            }
            .into(),
        };
        drop(kids);
        result
    }

    fn stmts(&mut self, list: a::List<a::StmtId>) -> Result<Vec<ast::Stmt>, Error> {
        let m = self.m;
        m.list(list).iter().map(|&id| self.stmt(id)).collect()
    }

    fn arguments(&mut self, id: a::ArgsId) -> ast::Arguments {
        let m = self.m;
        let args = *m.arguments(id);
        let posonlyargs = m
            .list(args.posonlyargs)
            .iter()
            .map(|p| self.param_with_default(p))
            .collect();
        let argsv = m
            .list(args.args)
            .iter()
            .map(|p| self.param_with_default(p))
            .collect();
        let vararg = args
            .vararg
            .map(|i| Box::new(self.param(&m.params[i as usize])));
        let kwonlyargs = m
            .list(args.kwonlyargs)
            .iter()
            .map(|p| self.param_with_default(p))
            .collect();
        let kwarg = args
            .kwarg
            .map(|i| Box::new(self.param(&m.params[i as usize])));
        ast::Arguments {
            posonlyargs,
            args: argsv,
            vararg,
            kwonlyargs,
            kwarg,
            range: opt(args.range),
        }
    }

    fn param(&mut self, p: &a::Param) -> ast::Arg {
        ast::Arg {
            arg: self.id(p.name),
            annotation: self.opt_expr(p.annotation),
            type_comment: None,
            range: tr(p.range),
        }
    }

    fn param_with_default(&mut self, p: &a::Param) -> ast::ArgWithDefault {
        ast::ArgWithDefault {
            def: self.param(p),
            default: self.opt_expr(p.default),
            range: opt(p.range),
        }
    }

    fn type_params(&mut self, list: a::List<a::TypeParam>) -> Result<Vec<ast::TypeParam>, Error> {
        let m = self.m;
        let mut out = Vec::with_capacity(list.len());
        for p in m.list(list) {
            if p.default.is_some() {
                return Err(Error::new(
                    "type parameter defaults (PEP 696) are not supported",
                    p.range.start,
                ));
            }
            let name = self.id(p.name);
            let range = tr(p.range);
            out.push(match p.kind {
                a::TypeParamKind::TypeVar { bound } => ast::TypeParamTypeVar {
                    name,
                    bound: self.opt_expr(bound),
                    range,
                }
                .into(),
                a::TypeParamKind::ParamSpec => ast::TypeParamParamSpec { name, range }.into(),
                a::TypeParamKind::TypeVarTuple => ast::TypeParamTypeVarTuple { name, range }.into(),
            });
        }
        Ok(out)
    }

    fn stmt(&mut self, id: a::StmtId) -> Result<ast::Stmt, Error> {
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
                let args = Box::new(self.arguments(args));
                let returns = self.opt_expr(returns);
                let body = self.stmts(body)?;
                let name = self.id(name);
                if is_async {
                    ast::StmtAsyncFunctionDef {
                        name,
                        args,
                        body,
                        decorator_list,
                        returns,
                        type_comment: None,
                        type_params,
                        range,
                    }
                    .into()
                } else {
                    ast::StmtFunctionDef {
                        name,
                        args,
                        body,
                        decorator_list,
                        returns,
                        type_comment: None,
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
                let keywords = m
                    .list(keywords)
                    .iter()
                    .map(|k| ast::Keyword {
                        arg: k.arg.map(|s| self.id(s)),
                        value: self.expr(k.value),
                        range: tr(k.range),
                    })
                    .collect();
                ast::StmtClassDef {
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
            StmtKind::Return { value } => ast::StmtReturn {
                value: self.opt_expr(value),
                range,
            }
            .into(),
            StmtKind::Delete { targets } => ast::StmtDelete {
                targets: self.exprs(targets),
                range,
            }
            .into(),
            StmtKind::Assign { targets, value } => ast::StmtAssign {
                targets: self.exprs(targets),
                value: Box::new(self.expr(value)),
                type_comment: None,
                range,
            }
            .into(),
            StmtKind::TypeAlias {
                name,
                type_params,
                value,
            } => {
                let name = Box::new(self.expr(name));
                let type_params = self.type_params(type_params)?;
                ast::StmtTypeAlias {
                    name,
                    type_params,
                    value: Box::new(self.expr(value)),
                    range,
                }
                .into()
            }
            StmtKind::AugAssign { target, op, value } => ast::StmtAugAssign {
                target: Box::new(self.expr(target)),
                op: operator(op),
                value: Box::new(self.expr(value)),
                range,
            }
            .into(),
            StmtKind::AnnAssign {
                target,
                annotation,
                value,
                simple,
            } => ast::StmtAnnAssign {
                target: Box::new(self.expr(target)),
                annotation: Box::new(self.expr(annotation)),
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
                let target = Box::new(self.expr(target));
                let iter = Box::new(self.expr(iter));
                let body = self.stmts(body)?;
                let orelse = self.stmts(orelse)?;
                if is_async {
                    ast::StmtAsyncFor {
                        target,
                        iter,
                        body,
                        orelse,
                        type_comment: None,
                        range,
                    }
                    .into()
                } else {
                    ast::StmtFor {
                        target,
                        iter,
                        body,
                        orelse,
                        type_comment: None,
                        range,
                    }
                    .into()
                }
            }
            StmtKind::While { test, body, orelse } => ast::StmtWhile {
                test: Box::new(self.expr(test)),
                body: self.stmts(body)?,
                orelse: self.stmts(orelse)?,
                range,
            }
            .into(),
            StmtKind::If { test, body, orelse } => ast::StmtIf {
                test: Box::new(self.expr(test)),
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
                let items = m
                    .list(items)
                    .iter()
                    .map(|i| ast::WithItem {
                        context_expr: self.expr(i.context_expr),
                        optional_vars: self.opt_expr(i.optional_vars),
                        range: opt(i.range),
                    })
                    .collect();
                let body = self.stmts(body)?;
                if is_async {
                    ast::StmtAsyncWith {
                        items,
                        body,
                        type_comment: None,
                        range,
                    }
                    .into()
                } else {
                    ast::StmtWith {
                        items,
                        body,
                        type_comment: None,
                        range,
                    }
                    .into()
                }
            }
            StmtKind::Match { subject, cases } => {
                let subject = Box::new(self.expr(subject));
                let mut out = Vec::with_capacity(cases.len());
                for case in m.list(cases) {
                    out.push(ast::MatchCase {
                        pattern: self.pattern(case.pattern),
                        guard: self.opt_expr(case.guard),
                        body: self.stmts(case.body)?,
                        range: opt(case.range),
                    });
                }
                ast::StmtMatch {
                    subject,
                    cases: out,
                    range,
                }
                .into()
            }
            StmtKind::Raise { exc, cause } => ast::StmtRaise {
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
                    hs.push(ast::ExceptHandler::ExceptHandler(
                        ast::ExceptHandlerExceptHandler {
                            type_: self.opt_expr(h.type_),
                            name: h.name.map(|s| self.id(s)),
                            body: self.stmts(h.body)?,
                            range: tr(h.range),
                        },
                    ));
                }
                let orelse = self.stmts(orelse)?;
                let finalbody = self.stmts(finalbody)?;
                if star {
                    ast::StmtTryStar {
                        body,
                        handlers: hs,
                        orelse,
                        finalbody,
                        range,
                    }
                    .into()
                } else {
                    ast::StmtTry {
                        body,
                        handlers: hs,
                        orelse,
                        finalbody,
                        range,
                    }
                    .into()
                }
            }
            StmtKind::Assert { test, msg } => ast::StmtAssert {
                test: Box::new(self.expr(test)),
                msg: self.opt_expr(msg),
                range,
            }
            .into(),
            StmtKind::Import { names } => ast::StmtImport {
                names: self.aliases(names),
                range,
            }
            .into(),
            StmtKind::ImportFrom {
                module,
                names,
                level,
            } => ast::StmtImportFrom {
                module: module.map(|s| self.id(s)),
                names: self.aliases(names),
                level: Some(ast::Int::new(level)),
                range,
            }
            .into(),
            StmtKind::Global { names } => ast::StmtGlobal {
                names: m.list(names).iter().map(|&s| self.id(s)).collect(),
                range,
            }
            .into(),
            StmtKind::Nonlocal { names } => ast::StmtNonlocal {
                names: m.list(names).iter().map(|&s| self.id(s)).collect(),
                range,
            }
            .into(),
            StmtKind::Expr { value } => ast::StmtExpr {
                value: Box::new(self.expr(value)),
                range,
            }
            .into(),
            StmtKind::Pass => ast::StmtPass { range }.into(),
            StmtKind::Break => ast::StmtBreak { range }.into(),
            StmtKind::Continue => ast::StmtContinue { range }.into(),
        })
    }

    fn aliases(&self, list: a::List<a::Alias>) -> Vec<ast::Alias> {
        self.m
            .list(list)
            .iter()
            .map(|alias| ast::Alias {
                name: self.id(alias.name),
                asname: alias.asname.map(|s| self.id(s)),
                range: tr(alias.range),
            })
            .collect()
    }

    fn pattern(&mut self, id: a::PatId) -> ast::Pattern {
        let m = self.m;
        let pattern = m.pattern(id);
        let range = tr(pattern.range);
        let pats = |c: &mut Self, list: a::List<a::PatId>| -> Vec<ast::Pattern> {
            m.list(list).iter().map(|&p| c.pattern(p)).collect()
        };
        match pattern.kind {
            PatternKind::MatchValue { value } => ast::PatternMatchValue {
                value: Box::new(self.expr(value)),
                range,
            }
            .into(),
            PatternKind::MatchSingleton { value } => ast::PatternMatchSingleton {
                value: match value {
                    a::Constant::True => ast::Constant::Bool(true),
                    a::Constant::False => ast::Constant::Bool(false),
                    _ => ast::Constant::None,
                },
                range,
            }
            .into(),
            PatternKind::MatchSequence { patterns } => ast::PatternMatchSequence {
                patterns: pats(self, patterns),
                range,
            }
            .into(),
            PatternKind::MatchMapping {
                keys,
                patterns,
                rest,
            } => ast::PatternMatchMapping {
                keys: self.exprs(keys),
                patterns: pats(self, patterns),
                rest: rest.map(|s| self.id(s)),
                range,
            }
            .into(),
            PatternKind::MatchClass {
                cls,
                patterns,
                kwd_attrs,
                kwd_patterns,
            } => ast::PatternMatchClass {
                cls: Box::new(self.expr(cls)),
                patterns: pats(self, patterns),
                kwd_attrs: m.list(kwd_attrs).iter().map(|&s| self.id(s)).collect(),
                kwd_patterns: pats(self, kwd_patterns),
                range,
            }
            .into(),
            PatternKind::MatchStar { name } => ast::PatternMatchStar {
                name: name.map(|s| self.id(s)),
                range,
            }
            .into(),
            PatternKind::MatchAs { pattern, name } => ast::PatternMatchAs {
                pattern: pattern.map(|p| Box::new(self.pattern(p))),
                name: name.map(|s| self.id(s)),
                range,
            }
            .into(),
            PatternKind::MatchOr { patterns } => ast::PatternMatchOr {
                patterns: pats(self, patterns),
                range,
            }
            .into(),
        }
    }
}

/// A lambda's parameters: defaults come from the converted children
/// (lambda parameters have no annotations).
fn lambda_arguments(
    m: &Module,
    id: a::ArgsId,
    next: &mut dyn FnMut() -> ast::Expr,
) -> ast::Arguments {
    let args = m.arguments(id);
    let arg = |p: &a::Param| ast::Arg {
        arg: ast::Identifier::new(m.name(p.name)),
        annotation: None,
        type_comment: None,
        range: tr(p.range),
    };
    let mut with_default = |p: &a::Param| ast::ArgWithDefault {
        def: arg(p),
        default: p.default.map(|_| Box::new(next())),
        range: opt(p.range),
    };
    let posonlyargs = m
        .list(args.posonlyargs)
        .iter()
        .map(&mut with_default)
        .collect();
    let argsv = m.list(args.args).iter().map(&mut with_default).collect();
    let kwonlyargs = m
        .list(args.kwonlyargs)
        .iter()
        .map(&mut with_default)
        .collect();
    ast::Arguments {
        posonlyargs,
        args: argsv,
        vararg: args.vararg.map(|i| Box::new(arg(&m.params[i as usize]))),
        kwonlyargs,
        kwarg: args.kwarg.map(|i| Box::new(arg(&m.params[i as usize]))),
        range: opt(args.range),
    }
}
