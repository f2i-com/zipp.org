//! Recursive-descent parser for the ZIPP v0 subset.

use crate::ast::*;
use crate::lexer::{Tok, Token};

pub fn parse(tokens: &[Token]) -> Result<Module, String> {
    let mut p = Parser { toks: tokens, pos: 0 };
    let mut funcs = Vec::new();
    while !p.at_end() {
        funcs.push(p.func()?);
    }
    Ok(Module { funcs })
}

struct Parser<'a> {
    toks: &'a [Token],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn at_end(&self) -> bool {
        self.pos >= self.toks.len()
    }

    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos).map(|t| &t.tok)
    }

    /// " (line L:C)" for the current token (or the last token at EOF).
    fn at(&self) -> String {
        match self.toks.get(self.pos).or_else(|| self.toks.last()) {
            Some(t) => format!(" (line {}:{})", t.line, t.col),
            None => String::new(),
        }
    }

    /// Line the current token starts on (used to tag statements).
    fn cur_line(&self) -> u32 {
        self.toks
            .get(self.pos)
            .or_else(|| self.toks.last())
            .map(|t| t.line)
            .unwrap_or(0)
    }

    fn bump(&mut self) -> Result<Tok, String> {
        let t = self
            .toks
            .get(self.pos)
            .map(|t| t.tok.clone())
            .ok_or_else(|| "parse error: unexpected end of input".to_string())?;
        self.pos += 1;
        Ok(t)
    }

    fn expect(&mut self, want: &Tok) -> Result<(), String> {
        let pos = self.at();
        let got = self.bump()?;
        if &got == want {
            Ok(())
        } else {
            Err(format!("parse error: expected {want:?}, found {got:?}{pos}"))
        }
    }

    fn eat(&mut self, want: &Tok) -> bool {
        if self.peek() == Some(want) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn ident(&mut self) -> Result<String, String> {
        let pos = self.at();
        match self.bump()? {
            Tok::Ident(s) => Ok(s),
            other => Err(format!("parse error: expected identifier, found {other:?}{pos}")),
        }
    }

    fn ty(&mut self) -> Result<Type, String> {
        match self.bump()? {
            Tok::TyI64 => Ok(Type::I64),
            Tok::TyF64 => Ok(Type::F64),
            Tok::TyBool => Ok(Type::Bool),
            Tok::TyStr => Ok(Type::Str),
            Tok::LBracket => {
                let inner = self.ty()?;
                self.expect(&Tok::RBracket)?;
                let elem = inner
                    .as_elem()
                    .ok_or("parse error: nested arrays are not supported in v0")?;
                Ok(Type::Array(elem))
            }
            other => Err(format!("parse error: expected type, found {other:?}")),
        }
    }

    fn func(&mut self) -> Result<Func, String> {
        self.expect(&Tok::Fn)?;
        let name = self.ident()?;
        self.expect(&Tok::LParen)?;
        let mut params = Vec::new();
        if self.peek() != Some(&Tok::RParen) {
            loop {
                let pname = self.ident()?;
                self.expect(&Tok::Colon)?;
                let pty = self.ty()?;
                params.push(Param { name: pname, ty: pty });
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
        }
        self.expect(&Tok::RParen)?;
        self.expect(&Tok::Colon)?;
        let ret = self.ty()?;
        let body = self.block()?;
        Ok(Func { name, params, ret, body })
    }

    fn block(&mut self) -> Result<Vec<Stmt>, String> {
        self.expect(&Tok::LBrace)?;
        let mut stmts = Vec::new();
        while self.peek() != Some(&Tok::RBrace) {
            if self.at_end() {
                return Err("parse error: unterminated block".into());
            }
            stmts.push(self.stmt()?);
        }
        self.expect(&Tok::RBrace)?;
        Ok(stmts)
    }

    fn stmt(&mut self) -> Result<Stmt, String> {
        let line = self.cur_line();
        let kind = self.stmt_kind()?;
        Ok(Stmt { kind, line })
    }

    fn stmt_kind(&mut self) -> Result<StmtKind, String> {
        match self.peek() {
            Some(Tok::Let) => {
                self.bump()?;
                let name = self.ident()?;
                let ty = if self.eat(&Tok::Colon) {
                    Some(self.ty()?)
                } else {
                    None
                };
                self.expect(&Tok::Assign)?;
                let value = self.expr()?;
                self.expect(&Tok::Semi)?;
                Ok(StmtKind::Let { name, ty, value })
            }
            Some(Tok::Return) => {
                self.bump()?;
                if self.eat(&Tok::Semi) {
                    Ok(StmtKind::Return(None))
                } else {
                    let e = self.expr()?;
                    self.expect(&Tok::Semi)?;
                    Ok(StmtKind::Return(Some(e)))
                }
            }
            Some(Tok::If) => {
                self.bump()?;
                self.expect(&Tok::LParen)?;
                let cond = self.expr()?;
                self.expect(&Tok::RParen)?;
                let then_b = self.block()?;
                let else_b = if self.eat(&Tok::Else) {
                    if self.peek() == Some(&Tok::If) {
                        // else if -> nest as a single-statement block
                        vec![self.stmt()?]
                    } else {
                        self.block()?
                    }
                } else {
                    Vec::new()
                };
                Ok(StmtKind::If { cond, then_b, else_b })
            }
            Some(Tok::While) => {
                self.bump()?;
                self.expect(&Tok::LParen)?;
                let cond = self.expr()?;
                self.expect(&Tok::RParen)?;
                let body = self.block()?;
                Ok(StmtKind::While { cond, body })
            }
            Some(Tok::For) => {
                self.bump()?;
                self.expect(&Tok::LParen)?;
                // init: a full statement (consumes its own `;`) or empty.
                let init = if self.eat(&Tok::Semi) {
                    None
                } else {
                    Some(Box::new(self.stmt()?))
                };
                let cond = self.expr()?;
                self.expect(&Tok::Semi)?;
                // step: assignment or expression, no trailing `;`.
                let step = if self.peek() == Some(&Tok::RParen) {
                    None
                } else {
                    Some(Box::new(self.simple_stmt()?))
                };
                self.expect(&Tok::RParen)?;
                let body = self.block()?;
                Ok(StmtKind::For { init, cond, step, body })
            }
            Some(Tok::Break) => {
                self.bump()?;
                self.expect(&Tok::Semi)?;
                Ok(StmtKind::Break)
            }
            Some(Tok::Continue) => {
                self.bump()?;
                self.expect(&Tok::Semi)?;
                Ok(StmtKind::Continue)
            }
            Some(Tok::Print) => {
                self.bump()?;
                self.expect(&Tok::LParen)?;
                let e = self.expr()?;
                self.expect(&Tok::RParen)?;
                self.expect(&Tok::Semi)?;
                Ok(StmtKind::Print(e))
            }
            // assignment to an lvalue (`x = e;` / `a[i] = e;`) or a bare expression
            _ => {
                let e = self.expr()?;
                if self.eat(&Tok::Assign) {
                    let value = self.expr()?;
                    self.expect(&Tok::Semi)?;
                    match e {
                        Expr::Var(_) | Expr::Index { .. } => Ok(StmtKind::Assign { target: e, value }),
                        _ => Err(format!("parse error: invalid assignment target{}", self.at())),
                    }
                } else {
                    self.expect(&Tok::Semi)?;
                    Ok(StmtKind::ExprStmt(e))
                }
            }
        }
    }

    /// A statement without a trailing semicolon — used for a `for` loop's step.
    fn simple_stmt(&mut self) -> Result<Stmt, String> {
        let line = self.cur_line();
        let e = self.expr()?;
        let kind = if self.eat(&Tok::Assign) {
            let value = self.expr()?;
            match e {
                Expr::Var(_) | Expr::Index { .. } => StmtKind::Assign { target: e, value },
                _ => return Err(format!("parse error: invalid assignment target{}", self.at())),
            }
        } else {
            StmtKind::ExprStmt(e)
        };
        Ok(Stmt { kind, line })
    }

    // ── expression precedence (low -> high) ──
    fn expr(&mut self) -> Result<Expr, String> {
        self.or_expr()
    }

    fn or_expr(&mut self) -> Result<Expr, String> {
        let mut l = self.and_expr()?;
        while self.eat(&Tok::OrOr) {
            let r = self.and_expr()?;
            l = Expr::Bin { op: BinOp::Or, l: Box::new(l), r: Box::new(r) };
        }
        Ok(l)
    }

    fn and_expr(&mut self) -> Result<Expr, String> {
        let mut l = self.bitor_expr()?;
        while self.eat(&Tok::AndAnd) {
            let r = self.bitor_expr()?;
            l = Expr::Bin { op: BinOp::And, l: Box::new(l), r: Box::new(r) };
        }
        Ok(l)
    }

    fn bitor_expr(&mut self) -> Result<Expr, String> {
        let mut l = self.bitxor_expr()?;
        while self.eat(&Tok::BitOr) {
            let r = self.bitxor_expr()?;
            l = Expr::Bin { op: BinOp::BitOr, l: Box::new(l), r: Box::new(r) };
        }
        Ok(l)
    }

    fn bitxor_expr(&mut self) -> Result<Expr, String> {
        let mut l = self.bitand_expr()?;
        while self.eat(&Tok::BitXor) {
            let r = self.bitand_expr()?;
            l = Expr::Bin { op: BinOp::BitXor, l: Box::new(l), r: Box::new(r) };
        }
        Ok(l)
    }

    fn bitand_expr(&mut self) -> Result<Expr, String> {
        let mut l = self.eq_expr()?;
        while self.eat(&Tok::BitAnd) {
            let r = self.eq_expr()?;
            l = Expr::Bin { op: BinOp::BitAnd, l: Box::new(l), r: Box::new(r) };
        }
        Ok(l)
    }

    fn eq_expr(&mut self) -> Result<Expr, String> {
        let mut l = self.cmp_expr()?;
        loop {
            let op = match self.peek() {
                Some(Tok::EqEq) => BinOp::Eq,
                Some(Tok::NotEq) => BinOp::Ne,
                _ => break,
            };
            self.bump()?;
            let r = self.cmp_expr()?;
            l = Expr::Bin { op, l: Box::new(l), r: Box::new(r) };
        }
        Ok(l)
    }

    fn cmp_expr(&mut self) -> Result<Expr, String> {
        let mut l = self.shift_expr()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Lt) => BinOp::Lt,
                Some(Tok::Le) => BinOp::Le,
                Some(Tok::Gt) => BinOp::Gt,
                Some(Tok::Ge) => BinOp::Ge,
                _ => break,
            };
            self.bump()?;
            let r = self.shift_expr()?;
            l = Expr::Bin { op, l: Box::new(l), r: Box::new(r) };
        }
        Ok(l)
    }

    fn shift_expr(&mut self) -> Result<Expr, String> {
        let mut l = self.add_expr()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Shl) => BinOp::Shl,
                Some(Tok::Shr) => BinOp::Shr,
                _ => break,
            };
            self.bump()?;
            let r = self.add_expr()?;
            l = Expr::Bin { op, l: Box::new(l), r: Box::new(r) };
        }
        Ok(l)
    }

    fn add_expr(&mut self) -> Result<Expr, String> {
        let mut l = self.mul_expr()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Plus) => BinOp::Add,
                Some(Tok::Minus) => BinOp::Sub,
                _ => break,
            };
            self.bump()?;
            let r = self.mul_expr()?;
            l = Expr::Bin { op, l: Box::new(l), r: Box::new(r) };
        }
        Ok(l)
    }

    fn mul_expr(&mut self) -> Result<Expr, String> {
        let mut l = self.unary_expr()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Star) => BinOp::Mul,
                Some(Tok::Slash) => BinOp::Div,
                Some(Tok::Percent) => BinOp::Mod,
                _ => break,
            };
            self.bump()?;
            let r = self.unary_expr()?;
            l = Expr::Bin { op, l: Box::new(l), r: Box::new(r) };
        }
        Ok(l)
    }

    fn unary_expr(&mut self) -> Result<Expr, String> {
        match self.peek() {
            Some(Tok::Minus) => {
                self.bump()?;
                let e = self.unary_expr()?;
                Ok(Expr::Unary { op: UnOp::Neg, e: Box::new(e) })
            }
            Some(Tok::Bang) => {
                self.bump()?;
                let e = self.unary_expr()?;
                Ok(Expr::Unary { op: UnOp::Not, e: Box::new(e) })
            }
            Some(Tok::BitNot) => {
                self.bump()?;
                let e = self.unary_expr()?;
                Ok(Expr::Unary { op: UnOp::BitNot, e: Box::new(e) })
            }
            _ => self.postfix(),
        }
    }

    /// Primary followed by zero or more `[index]` suffixes.
    fn postfix(&mut self) -> Result<Expr, String> {
        let mut e = self.primary()?;
        while self.peek() == Some(&Tok::LBracket) {
            self.bump()?;
            let index = self.expr()?;
            self.expect(&Tok::RBracket)?;
            e = Expr::Index { arr: Box::new(e), index: Box::new(index) };
        }
        Ok(e)
    }

    fn primary(&mut self) -> Result<Expr, String> {
        let pos = self.at();
        match self.bump()? {
            Tok::Int(n) => Ok(Expr::Int(n)),
            Tok::Float(f) => Ok(Expr::Float(f)),
            Tok::Str(s) => Ok(Expr::Str(s)),
            Tok::True => Ok(Expr::Bool(true)),
            Tok::False => Ok(Expr::Bool(false)),
            // Casts: `i64(expr)` / `f64(expr)`.
            Tok::TyI64 => {
                self.expect(&Tok::LParen)?;
                let e = self.expr()?;
                self.expect(&Tok::RParen)?;
                Ok(Expr::Cast { to: Type::I64, e: Box::new(e) })
            }
            Tok::TyF64 => {
                self.expect(&Tok::LParen)?;
                let e = self.expr()?;
                self.expect(&Tok::RParen)?;
                Ok(Expr::Cast { to: Type::F64, e: Box::new(e) })
            }
            Tok::LParen => {
                let e = self.expr()?;
                self.expect(&Tok::RParen)?;
                Ok(e)
            }
            Tok::LBracket => {
                if self.peek() == Some(&Tok::RBracket) {
                    return Err("parse error: empty array literal needs a type — use `[value; 0]`".into());
                }
                let first = self.expr()?;
                if self.eat(&Tok::Semi) {
                    // repeat literal [value; count]
                    let count = self.expr()?;
                    self.expect(&Tok::RBracket)?;
                    Ok(Expr::Repeat { value: Box::new(first), count: Box::new(count) })
                } else {
                    let mut elems = vec![first];
                    while self.eat(&Tok::Comma) {
                        if self.peek() == Some(&Tok::RBracket) {
                            break; // tolerate trailing comma
                        }
                        elems.push(self.expr()?);
                    }
                    self.expect(&Tok::RBracket)?;
                    Ok(Expr::Array(elems))
                }
            }
            Tok::Ident(name) => {
                if self.eat(&Tok::LParen) {
                    let mut args = Vec::new();
                    if self.peek() != Some(&Tok::RParen) {
                        loop {
                            args.push(self.expr()?);
                            if !self.eat(&Tok::Comma) {
                                break;
                            }
                        }
                    }
                    self.expect(&Tok::RParen)?;
                    Ok(Expr::Call { name, args })
                } else {
                    Ok(Expr::Var(name))
                }
            }
            other => Err(format!("parse error: unexpected token {other:?}{pos}")),
        }
    }
}
