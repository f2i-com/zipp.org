//! Recursive-descent parser for the ZIPP v0 subset.

use crate::ast::*;
use crate::lexer::Tok;

pub fn parse(tokens: &[Tok]) -> Result<Module, String> {
    let mut p = Parser { toks: tokens, pos: 0 };
    let mut funcs = Vec::new();
    while !p.at_end() {
        funcs.push(p.func()?);
    }
    Ok(Module { funcs })
}

struct Parser<'a> {
    toks: &'a [Tok],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn at_end(&self) -> bool {
        self.pos >= self.toks.len()
    }

    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn bump(&mut self) -> Result<Tok, String> {
        let t = self
            .toks
            .get(self.pos)
            .cloned()
            .ok_or_else(|| "parse error: unexpected end of input".to_string())?;
        self.pos += 1;
        Ok(t)
    }

    fn expect(&mut self, want: &Tok) -> Result<(), String> {
        let got = self.bump()?;
        if &got == want {
            Ok(())
        } else {
            Err(format!("parse error: expected {want:?}, found {got:?}"))
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
        match self.bump()? {
            Tok::Ident(s) => Ok(s),
            other => Err(format!("parse error: expected identifier, found {other:?}")),
        }
    }

    fn ty(&mut self) -> Result<Type, String> {
        match self.bump()? {
            Tok::TyI64 => Ok(Type::I64),
            Tok::TyBool => Ok(Type::Bool),
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
                Ok(Stmt::Let { name, ty, value })
            }
            Some(Tok::Return) => {
                self.bump()?;
                if self.eat(&Tok::Semi) {
                    Ok(Stmt::Return(None))
                } else {
                    let e = self.expr()?;
                    self.expect(&Tok::Semi)?;
                    Ok(Stmt::Return(Some(e)))
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
                Ok(Stmt::If { cond, then_b, else_b })
            }
            Some(Tok::While) => {
                self.bump()?;
                self.expect(&Tok::LParen)?;
                let cond = self.expr()?;
                self.expect(&Tok::RParen)?;
                let body = self.block()?;
                Ok(Stmt::While { cond, body })
            }
            Some(Tok::Break) => {
                self.bump()?;
                self.expect(&Tok::Semi)?;
                Ok(Stmt::Break)
            }
            Some(Tok::Continue) => {
                self.bump()?;
                self.expect(&Tok::Semi)?;
                Ok(Stmt::Continue)
            }
            Some(Tok::Print) => {
                self.bump()?;
                self.expect(&Tok::LParen)?;
                let e = self.expr()?;
                self.expect(&Tok::RParen)?;
                self.expect(&Tok::Semi)?;
                Ok(Stmt::Print(e))
            }
            // assignment `name = expr;` or a bare expression statement
            Some(Tok::Ident(_)) if self.toks.get(self.pos + 1) == Some(&Tok::Assign) => {
                let name = self.ident()?;
                self.expect(&Tok::Assign)?;
                let value = self.expr()?;
                self.expect(&Tok::Semi)?;
                Ok(Stmt::Assign { name, value })
            }
            _ => {
                let e = self.expr()?;
                self.expect(&Tok::Semi)?;
                Ok(Stmt::ExprStmt(e))
            }
        }
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
            _ => self.primary(),
        }
    }

    fn primary(&mut self) -> Result<Expr, String> {
        match self.bump()? {
            Tok::Int(n) => Ok(Expr::Int(n)),
            Tok::True => Ok(Expr::Bool(true)),
            Tok::False => Ok(Expr::Bool(false)),
            Tok::LParen => {
                let e = self.expr()?;
                self.expect(&Tok::RParen)?;
                Ok(e)
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
            other => Err(format!("parse error: unexpected token {other:?}")),
        }
    }
}
