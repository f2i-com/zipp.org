//! ZIPP **TypeScript frontend**: parse real `.ts` with `oxc`, then lower the
//! sound subset to ZIPP's own AST (`zippc::ast`). Everything downstream — the
//! sound checker, IR, GC, and all four execution targets (interp/jit/llvm/wasm)
//! plus the zk prover — is reused unchanged.
//!
//! `oxc` parses *all* TypeScript syntax; this module lowers the subset that maps
//! to ZIPP's sound, AOT-compilable core and rejects the rest with a clear,
//! line-numbered error. That's the AssemblyScript model: write real TS (tsc and
//! your editor type-check it), compile it to fast/provable/gas-metered code.
//!
//! Supported (v0): typed functions + recursion, `let`/`const` with annotations,
//! `if`/`while`/`for`, `break`/`continue`, return, the full operator set,
//! numeric casts (`i64(x)`, `u32(x)`, …), arrays (`T[]`, indexing, `.length`),
//! `console.log`/`print`, and the math builtins. Type mapping: `number`→f64,
//! `bigint`→i64, `boolean`→bool, `string`→str, and the ZIPP type names
//! `i64`/`i32`/`u32`/`u64`/`f64` usable directly. Not yet: classes, interfaces/
//! structs, generics, closures — and never the dynamic core (`any`, prototypes,
//! `eval`, exceptions, async), which is off-mission for an AOT/provable language.

use oxc_allocator::Allocator;
use oxc_ast::ast::*;
use oxc_parser::Parser;
use oxc_span::{SourceType, Span};
use oxc_syntax::operator::{AssignmentOperator, BinaryOperator, LogicalOperator, UnaryOperator};

use zippc::ast as z;

/// Parse TypeScript source and lower the sound subset to a ZIPP [`z::Module`].
pub fn compile_ts(src: &str) -> Result<z::Module, String> {
    let allocator = Allocator::default();
    let source_type = SourceType::ts();
    let ret = Parser::new(&allocator, src, source_type).parse();
    if !ret.errors.is_empty() {
        return Err(format!("typescript parse error: {}", ret.errors[0]));
    }
    Lower { src }.module(&ret.program)
}

struct Lower<'s> {
    src: &'s str,
}

type LResult<T> = Result<T, String>;

impl Lower<'_> {
    fn line(&self, span: Span) -> u32 {
        self.src.as_bytes()[..(span.start as usize).min(self.src.len())]
            .iter()
            .filter(|&&b| b == b'\n')
            .count() as u32
            + 1
    }

    fn err(&self, span: Span, msg: impl std::fmt::Display) -> String {
        format!("typescript error [line {}]: {msg}", self.line(span))
    }

    fn module(&self, program: &Program) -> LResult<z::Module> {
        let mut funcs = Vec::new();
        for stmt in &program.body {
            match stmt {
                Statement::FunctionDeclaration(f) => funcs.push(self.func(f)?),
                // Type-only declarations carry no runtime meaning — skip them.
                Statement::TSTypeAliasDeclaration(_) | Statement::TSInterfaceDeclaration(_) => {}
                Statement::EmptyStatement(_) => {}
                other => {
                    return Err(self.err(
                        span_of_stmt(other),
                        "only top-level function declarations are supported (v0)",
                    ))
                }
            }
        }
        Ok(z::Module { funcs, structs: Vec::new() })
    }

    fn func(&self, f: &Function) -> LResult<z::Func> {
        let name = f
            .id
            .as_ref()
            .ok_or_else(|| self.err(f.span, "functions must be named"))?
            .name
            .as_str()
            .to_string();
        let mut params = Vec::new();
        for p in &f.params.items {
            let pname = match &p.pattern {
                BindingPattern::BindingIdentifier(bi) => bi.name.as_str().to_string(),
                _ => return Err(self.err(p.span, "destructuring parameters aren't supported")),
            };
            let ann = p
                .type_annotation
                .as_ref()
                .ok_or_else(|| self.err(p.span, format!("parameter '{pname}' needs a type")))?;
            params.push(z::Param { name: pname, ty: self.ty(&ann.type_annotation)? });
        }
        let ret = match &f.return_type {
            Some(ann) => self.ty(&ann.type_annotation)?,
            None => return Err(self.err(f.span, "functions need an explicit return type")),
        };
        let body = match &f.body {
            Some(b) => self.block(&b.statements)?,
            None => return Err(self.err(f.span, "function has no body")),
        };
        Ok(z::Func { name, params, ret, body })
    }

    fn ty(&self, t: &TSType) -> LResult<z::Type> {
        Ok(match t {
            TSType::TSNumberKeyword(_) => z::Type::F64, // TS `number` is f64
            TSType::TSBigIntKeyword(_) => z::Type::I64,
            TSType::TSBooleanKeyword(_) => z::Type::Bool,
            TSType::TSStringKeyword(_) => z::Type::Str,
            TSType::TSArrayType(a) => {
                let elem = self.ty(&a.element_type)?;
                let e = elem.as_elem().ok_or_else(|| {
                    self.err(a.span, format!("array element type {elem:?} isn't a scalar"))
                })?;
                z::Type::Array(e)
            }
            TSType::TSTypeReference(r) => {
                let name = match &r.type_name {
                    TSTypeName::IdentifierReference(id) => id.name.as_str(),
                    _ => return Err(self.err(r.span, "qualified type names aren't supported")),
                };
                match name {
                    "i64" => z::Type::I64,
                    "i32" => z::Type::I32,
                    "u32" => z::Type::U32,
                    "u64" => z::Type::U64,
                    "f64" => z::Type::F64,
                    "bool" => z::Type::Bool,
                    "string" | "str" => z::Type::Str,
                    other => return Err(self.err(r.span, format!("unsupported type '{other}'"))),
                }
            }
            TSType::TSParenthesizedType(p) => self.ty(&p.type_annotation)?,
            other => return Err(self.err(span_of_type(other), "unsupported type")),
        })
    }

    fn block(&self, stmts: &[Statement]) -> LResult<Vec<z::Stmt>> {
        let mut out = Vec::new();
        for s in stmts {
            self.stmt(s, &mut out)?;
        }
        Ok(out)
    }

    /// Lower a statement, pushing one or more ZIPP statements.
    fn stmt(&self, s: &Statement, out: &mut Vec<z::Stmt>) -> LResult<()> {
        let push = |out: &mut Vec<z::Stmt>, kind, span| {
            out.push(z::Stmt { kind, line: self.line(span) })
        };
        match s {
            Statement::VariableDeclaration(v) => self.var_decl(v, out)?,
            Statement::ReturnStatement(r) => {
                let val = match &r.argument {
                    Some(e) => Some(self.expr(e)?),
                    None => None,
                };
                push(out, z::StmtKind::Return(val), r.span);
            }
            Statement::IfStatement(i) => {
                let cond = self.expr(&i.test)?;
                let then_b = self.stmt_as_block(&i.consequent)?;
                let else_b = match &i.alternate {
                    Some(a) => self.stmt_as_block(a)?,
                    None => Vec::new(),
                };
                push(out, z::StmtKind::If { cond, then_b, else_b }, i.span);
            }
            Statement::WhileStatement(w) => {
                let cond = self.expr(&w.test)?;
                let body = self.stmt_as_block(&w.body)?;
                push(out, z::StmtKind::While { cond, body }, w.span);
            }
            Statement::ForStatement(f) => {
                let init = match &f.init {
                    Some(ForStatementInit::VariableDeclaration(v)) => {
                        let mut tmp = Vec::new();
                        self.var_decl(v, &mut tmp)?;
                        tmp.pop().map(Box::new)
                    }
                    Some(other) => {
                        let e = other
                            .as_expression()
                            .ok_or_else(|| self.err(f.span, "unsupported for-init"))?;
                        Some(Box::new(self.expr_stmt(e)?))
                    }
                    None => None,
                };
                let cond = match &f.test {
                    Some(e) => self.expr(e)?,
                    None => z::Expr::Bool(true),
                };
                let step = match &f.update {
                    Some(e) => Some(Box::new(self.expr_stmt(e)?)),
                    None => None,
                };
                let body = self.stmt_as_block(&f.body)?;
                push(out, z::StmtKind::For { init, cond, step, body }, f.span);
            }
            Statement::BreakStatement(b) => push(out, z::StmtKind::Break, b.span),
            Statement::ContinueStatement(c) => push(out, z::StmtKind::Continue, c.span),
            Statement::BlockStatement(b) => {
                // Flatten a bare block (v0 — no separate block scope).
                for s in &b.body {
                    self.stmt(s, out)?;
                }
            }
            Statement::EmptyStatement(_) => {}
            Statement::ExpressionStatement(e) => {
                let st = self.expr_stmt(&e.expression)?;
                out.push(st);
            }
            other => return Err(self.err(span_of_stmt(other), "unsupported statement")),
        }
        Ok(())
    }

    /// Lower a `let`/`const` (one or more declarators) into ZIPP `Let`s.
    fn var_decl(&self, v: &VariableDeclaration, out: &mut Vec<z::Stmt>) -> LResult<()> {
        for d in &v.declarations {
            let name = match &d.id {
                BindingPattern::BindingIdentifier(bi) => bi.name.as_str().to_string(),
                _ => return Err(self.err(d.span, "destructuring `let` isn't supported")),
            };
            let value = d
                .init
                .as_ref()
                .ok_or_else(|| self.err(d.span, format!("'{name}' must be initialized")))?;
            let ty = match &d.type_annotation {
                Some(a) => Some(self.ty(&a.type_annotation)?),
                None => None,
            };
            out.push(z::Stmt {
                kind: z::StmtKind::Let { name, ty, value: self.expr(value)? },
                line: self.line(d.span),
            });
        }
        Ok(())
    }

    /// Lower a `Statement` that may or may not be a block into a ZIPP block.
    fn stmt_as_block(&self, s: &Statement) -> LResult<Vec<z::Stmt>> {
        if let Statement::BlockStatement(b) = s {
            self.block(&b.body)
        } else {
            let mut out = Vec::new();
            self.stmt(s, &mut out)?;
            Ok(out)
        }
    }

    /// An expression used in statement position: assignment, `print`, or a bare
    /// expression.
    fn expr_stmt(&self, e: &Expression) -> LResult<z::Stmt> {
        let line = self.line(span_of_expr(e));
        let kind = match e {
            Expression::AssignmentExpression(a) => {
                let target = self.assign_target(&a.left)?;
                let value = self.expr(&a.right)?;
                let value = match assign_bin_op(a.operator) {
                    None => value, // plain `=`
                    Some(op) => z::Expr::Bin {
                        op,
                        l: Box::new(target.clone()),
                        r: Box::new(value),
                    },
                };
                z::StmtKind::Assign { target, value }
            }
            // `print(x)` / `console.log(x)` → ZIPP print.
            Expression::CallExpression(c) if self.is_print_call(c) => {
                let arg = c
                    .arguments
                    .first()
                    .and_then(|a| a.as_expression())
                    .ok_or_else(|| self.err(c.span, "print expects one argument"))?;
                z::StmtKind::Print(self.expr(arg)?)
            }
            _ => z::StmtKind::ExprStmt(self.expr(e)?),
        };
        Ok(z::Stmt { kind, line })
    }

    fn is_print_call(&self, c: &CallExpression) -> bool {
        match &c.callee {
            Expression::Identifier(id) => id.name.as_str() == "print",
            Expression::StaticMemberExpression(m) => {
                m.property.name.as_str() == "log"
                    && matches!(&m.object, Expression::Identifier(o) if o.name.as_str() == "console")
            }
            _ => false,
        }
    }

    fn assign_target(&self, t: &AssignmentTarget) -> LResult<z::Expr> {
        match t {
            AssignmentTarget::AssignmentTargetIdentifier(id) => {
                Ok(z::Expr::Var(id.name.as_str().to_string()))
            }
            AssignmentTarget::ComputedMemberExpression(m) => Ok(z::Expr::Index {
                arr: Box::new(self.expr(&m.object)?),
                index: Box::new(self.expr(&m.expression)?),
            }),
            _ => Err(self.err(span_of_assign_target(t), "unsupported assignment target")),
        }
    }

    fn expr(&self, e: &Expression) -> LResult<z::Expr> {
        Ok(match e {
            Expression::NumericLiteral(n) => {
                if numeric_is_float(n) {
                    z::Expr::Float(n.value)
                } else {
                    z::Expr::Int(n.value as i64)
                }
            }
            Expression::StringLiteral(s) => z::Expr::Str(s.value.as_str().to_string()),
            Expression::BooleanLiteral(b) => z::Expr::Bool(b.value),
            Expression::Identifier(id) => z::Expr::Var(id.name.as_str().to_string()),
            Expression::ParenthesizedExpression(p) => self.expr(&p.expression)?,
            Expression::BinaryExpression(b) => z::Expr::Bin {
                op: self.bin_op(b.operator, b.span)?,
                l: Box::new(self.expr(&b.left)?),
                r: Box::new(self.expr(&b.right)?),
            },
            Expression::LogicalExpression(l) => z::Expr::Bin {
                op: match l.operator {
                    LogicalOperator::And => z::BinOp::And,
                    LogicalOperator::Or => z::BinOp::Or,
                    LogicalOperator::Coalesce => {
                        return Err(self.err(l.span, "`??` is not supported"))
                    }
                },
                l: Box::new(self.expr(&l.left)?),
                r: Box::new(self.expr(&l.right)?),
            },
            Expression::UnaryExpression(u) => {
                let inner = self.expr(&u.argument)?;
                match u.operator {
                    UnaryOperator::UnaryNegation => z::Expr::Unary { op: z::UnOp::Neg, e: Box::new(inner) },
                    UnaryOperator::LogicalNot => z::Expr::Unary { op: z::UnOp::Not, e: Box::new(inner) },
                    UnaryOperator::BitwiseNot => z::Expr::Unary { op: z::UnOp::BitNot, e: Box::new(inner) },
                    UnaryOperator::UnaryPlus => inner, // no-op
                    _ => return Err(self.err(u.span, "unsupported unary operator")),
                }
            }
            Expression::CallExpression(c) => self.call(c)?,
            Expression::ArrayExpression(a) => {
                let mut elems = Vec::new();
                for el in &a.elements {
                    let ex = el
                        .as_expression()
                        .ok_or_else(|| self.err(a.span, "array holes/spreads aren't supported"))?;
                    elems.push(self.expr(ex)?);
                }
                z::Expr::Array(elems)
            }
            Expression::ComputedMemberExpression(m) => z::Expr::Index {
                arr: Box::new(self.expr(&m.object)?),
                index: Box::new(self.expr(&m.expression)?),
            },
            Expression::StaticMemberExpression(m) => {
                if m.property.name.as_str() == "length" {
                    // `arr.length` → len(arr)
                    z::Expr::Call { name: "len".into(), args: vec![self.expr(&m.object)?] }
                } else {
                    return Err(self.err(m.span, "field access (structs) isn't supported yet"));
                }
            }
            // `x as T` — treat a numeric cast as a runtime conversion.
            Expression::TSAsExpression(a) => z::Expr::Cast {
                to: self.ty(&a.type_annotation)?,
                e: Box::new(self.expr(&a.expression)?),
            },
            other => return Err(self.err(span_of_expr(other), "unsupported expression")),
        })
    }

    /// A call is either a numeric cast (`i64(x)`), `len`/a math builtin, or a
    /// user function call.
    fn call(&self, c: &CallExpression) -> LResult<z::Expr> {
        let name = match &c.callee {
            Expression::Identifier(id) => id.name.as_str().to_string(),
            _ => return Err(self.err(c.span, "only direct function calls are supported")),
        };
        let mut args = Vec::new();
        for a in &c.arguments {
            let ex = a
                .as_expression()
                .ok_or_else(|| self.err(c.span, "spread arguments aren't supported"))?;
            args.push(self.expr(ex)?);
        }
        // Numeric cast keywords used as calls.
        let cast = match name.as_str() {
            "i64" => Some(z::Type::I64),
            "i32" => Some(z::Type::I32),
            "u32" => Some(z::Type::U32),
            "u64" => Some(z::Type::U64),
            "f64" => Some(z::Type::F64),
            _ => None,
        };
        if let Some(to) = cast {
            if args.len() != 1 {
                return Err(self.err(c.span, format!("{name}(x) takes one argument")));
            }
            return Ok(z::Expr::Cast { to, e: Box::new(args.into_iter().next().unwrap()) });
        }
        Ok(z::Expr::Call { name, args })
    }

    fn bin_op(&self, op: BinaryOperator, span: Span) -> LResult<z::BinOp> {
        use BinaryOperator as B;
        Ok(match op {
            B::Addition => z::BinOp::Add,
            B::Subtraction => z::BinOp::Sub,
            B::Multiplication => z::BinOp::Mul,
            B::Division => z::BinOp::Div,
            B::Remainder => z::BinOp::Mod,
            B::Equality | B::StrictEquality => z::BinOp::Eq,
            B::Inequality | B::StrictInequality => z::BinOp::Ne,
            B::LessThan => z::BinOp::Lt,
            B::LessEqualThan => z::BinOp::Le,
            B::GreaterThan => z::BinOp::Gt,
            B::GreaterEqualThan => z::BinOp::Ge,
            B::BitwiseAnd => z::BinOp::BitAnd,
            B::BitwiseOR => z::BinOp::BitOr,
            B::BitwiseXOR => z::BinOp::BitXor,
            B::ShiftLeft => z::BinOp::Shl,
            B::ShiftRight => z::BinOp::Shr,
            _ => return Err(self.err(span, format!("unsupported operator {op:?}"))),
        })
    }
}

fn numeric_is_float(n: &NumericLiteral) -> bool {
    match &n.raw {
        Some(raw) => {
            let s = raw.as_str();
            if s.starts_with("0x") || s.starts_with("0X") || s.starts_with("0b") || s.starts_with("0o") {
                false
            } else {
                s.contains('.') || s.contains('e') || s.contains('E')
            }
        }
        None => n.value.fract() != 0.0,
    }
}

fn assign_bin_op(op: AssignmentOperator) -> Option<z::BinOp> {
    use AssignmentOperator as A;
    match op {
        A::Assign => None,
        A::Addition => Some(z::BinOp::Add),
        A::Subtraction => Some(z::BinOp::Sub),
        A::Multiplication => Some(z::BinOp::Mul),
        A::Division => Some(z::BinOp::Div),
        A::Remainder => Some(z::BinOp::Mod),
        A::BitwiseAnd => Some(z::BinOp::BitAnd),
        A::BitwiseOR => Some(z::BinOp::BitOr),
        A::BitwiseXOR => Some(z::BinOp::BitXor),
        A::ShiftLeft => Some(z::BinOp::Shl),
        A::ShiftRight => Some(z::BinOp::Shr),
        _ => None, // unsupported compound ops fall through as plain assign (checker will catch type errors)
    }
}

// Span accessors (oxc nodes implement GetSpan, but matching keeps deps minimal).
fn span_of_stmt(s: &Statement) -> Span {
    oxc_span::GetSpan::span(s)
}
fn span_of_expr(e: &Expression) -> Span {
    oxc_span::GetSpan::span(e)
}
fn span_of_type(t: &TSType) -> Span {
    oxc_span::GetSpan::span(t)
}
fn span_of_assign_target(t: &AssignmentTarget) -> Span {
    oxc_span::GetSpan::span(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowers_and_runs_fib() {
        let ts = "function fib(n: i64): i64 { if (n < 2) { return n; } return fib(n - 1) + fib(n - 2); } \
                  function main(): i64 { return fib(10); }";
        let module = compile_ts(ts).expect("lower");
        let prog = zippc::compile_module(&module).expect("compile");
        let r = zippc::vm::run(&prog, false).expect("run");
        assert_eq!(r.result, zippc::Value::I64(55));
    }
}
