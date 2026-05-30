//! The tree-walking evaluator.
//!
//! Statements return a [`Flow`] (normal / return / break / continue); the `Err`
//! channel of [`EvalResult`] carries a *thrown* JS value (so `throw`/`try` and
//! engine errors share one mechanism). Expressions evaluate to a [`JsValue`].

use std::cell::RefCell;
use std::rc::Rc;

use crate::ast::*;
use crate::env::{self, Scope};
use crate::value::{JsValue, NativeFn, ObjData, Object};

/// `Ok` = a value; `Err` = a thrown JS value (an `Error` object or anything).
pub type EvalResult<T> = Result<T, JsValue>;

/// Statement completion / non-local control flow.
pub enum Flow {
    Normal,
    Return(JsValue),
    Break,
    Continue,
}

pub struct Interp {
    pub global: Rc<RefCell<Scope>>,
    /// Captured `console` output (one entry per `console.log` call).
    pub out: RefCell<Vec<String>>,
}

impl Default for Interp {
    fn default() -> Self {
        Self::new()
    }
}

impl Interp {
    pub fn new() -> Interp {
        let it = Interp { global: Scope::global(), out: RefCell::new(Vec::new()) };
        crate::builtins::install(&it);
        it
    }

    /// Run a whole program at global scope.
    pub fn run(&self, prog: &[Stmt]) -> EvalResult<()> {
        let scope = self.global.clone();
        self.hoist(prog, &scope);
        for s in prog {
            self.exec(s, &scope)?;
        }
        Ok(())
    }

    /// Hoist function declarations into `scope` so they're callable before their
    /// textual position (and mutually recursive). (`var` hoisting is approximated
    /// — `var` behaves like a function-scoped `let` here.)
    fn hoist(&self, stmts: &[Stmt], scope: &Rc<RefCell<Scope>>) {
        for s in stmts {
            if let Stmt::Func(def) = s {
                if let Some(name) = &def.name {
                    let f = JsValue::Object(Object::function(def.clone(), scope.clone()));
                    scope.borrow_mut().declare(name, f);
                }
            }
        }
    }

    // ───────────────────────── statements ─────────────────────────

    fn exec(&self, stmt: &Stmt, scope: &Rc<RefCell<Scope>>) -> EvalResult<Flow> {
        match stmt {
            Stmt::Empty | Stmt::Func(_) => Ok(Flow::Normal),
            Stmt::Class(cd) => {
                // The constructor + methods live in a class scope that holds
                // `%super%` (the parent constructor), so `super(...)`/`super.m()`
                // resolve from within them.
                let cscope = Scope::child(scope);
                let sup = match &cd.superclass {
                    Some(e) => Some(self.eval(e, scope)?),
                    None => None,
                };
                if let Some(s) = &sup {
                    cscope.borrow_mut().declare("%super%", s.clone());
                }
                let ctor = JsValue::Object(Object::function(cd.ctor.clone(), cscope.clone()));
                let proto = self.get_member(&ctor, "prototype")?; // vivifies prototype obj
                if let Some(s) = &sup {
                    // D.prototype.[[Prototype]] = B.prototype (instance/method
                    // inheritance); D.[[Prototype]] = B (static inheritance).
                    let sproto = self.get_member(s, "prototype")?;
                    if let (JsValue::Object(p), JsValue::Object(sp)) = (&proto, &sproto) {
                        p.borrow_mut().proto = Some(sp.clone());
                    }
                    if let (JsValue::Object(c), JsValue::Object(sc)) = (&ctor, s) {
                        c.borrow_mut().proto = Some(sc.clone());
                    }
                }
                for (mname, fd) in &cd.methods {
                    let m = JsValue::Object(Object::function(fd.clone(), cscope.clone()));
                    self.set_member(&proto, mname, m)?;
                }
                for (sname, fd) in &cd.statics {
                    let m = JsValue::Object(Object::function(fd.clone(), cscope.clone()));
                    self.set_member(&ctor, sname, m)?;
                }
                scope.borrow_mut().declare(&cd.name, ctor);
                Ok(Flow::Normal)
            }
            Stmt::Expr(e) => {
                self.eval(e, scope)?;
                Ok(Flow::Normal)
            }
            Stmt::Var { decls, .. } => {
                for (name, init) in decls {
                    let v = match init {
                        Some(e) => self.eval(e, scope)?,
                        None => JsValue::Undefined,
                    };
                    scope.borrow_mut().declare(name, v);
                }
                Ok(Flow::Normal)
            }
            Stmt::Block(stmts) => {
                let inner = Scope::child(scope);
                self.hoist(stmts, &inner);
                self.exec_block(stmts, &inner)
            }
            Stmt::If { cond, then, els } => {
                if self.eval(cond, scope)?.truthy() {
                    self.exec(then, scope)
                } else if let Some(e) = els {
                    self.exec(e, scope)
                } else {
                    Ok(Flow::Normal)
                }
            }
            Stmt::While { cond, body } => {
                while self.eval(cond, scope)?.truthy() {
                    match self.exec(body, scope)? {
                        Flow::Break => break,
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        _ => {}
                    }
                }
                Ok(Flow::Normal)
            }
            Stmt::DoWhile { body, cond } => {
                loop {
                    match self.exec(body, scope)? {
                        Flow::Break => break,
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        _ => {}
                    }
                    if !self.eval(cond, scope)?.truthy() {
                        break;
                    }
                }
                Ok(Flow::Normal)
            }
            Stmt::For { init, cond, step, body } => {
                let s = Scope::child(scope);
                if let Some(init) = init {
                    self.exec(init, &s)?;
                }
                loop {
                    if let Some(c) = cond {
                        if !self.eval(c, &s)?.truthy() {
                            break;
                        }
                    }
                    match self.exec(body, &s)? {
                        Flow::Break => break,
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        _ => {}
                    }
                    if let Some(st) = step {
                        self.eval(st, &s)?;
                    }
                }
                Ok(Flow::Normal)
            }
            Stmt::Return(e) => {
                let v = match e {
                    Some(e) => self.eval(e, scope)?,
                    None => JsValue::Undefined,
                };
                Ok(Flow::Return(v))
            }
            Stmt::Break => Ok(Flow::Break),
            Stmt::Continue => Ok(Flow::Continue),
            Stmt::Throw(e) => Err(self.eval(e, scope)?),
            Stmt::Try { block, catch, finally } => {
                let inner = Scope::child(scope);
                self.hoist(block, &inner);
                let mut result = self.exec_block(block, &inner);
                if let Err(thrown) = result {
                    if let Some((binding, cbody)) = catch {
                        let cs = Scope::child(scope);
                        if let Some(name) = binding {
                            cs.borrow_mut().declare(name, thrown);
                        }
                        self.hoist(cbody, &cs);
                        result = self.exec_block(cbody, &cs);
                    } else {
                        result = Err(thrown);
                    }
                }
                if let Some(fbody) = finally {
                    let fs = Scope::child(scope);
                    self.hoist(fbody, &fs);
                    match self.exec_block(fbody, &fs)? {
                        Flow::Normal => {}
                        other => return Ok(other), // finally's control flow wins
                    }
                }
                result
            }
        }
    }

    fn exec_block(&self, stmts: &[Stmt], scope: &Rc<RefCell<Scope>>) -> EvalResult<Flow> {
        for s in stmts {
            match self.exec(s, scope)? {
                Flow::Normal => {}
                other => return Ok(other),
            }
        }
        Ok(Flow::Normal)
    }

    // ───────────────────────── expressions ─────────────────────────

    pub fn eval(&self, e: &Expr, scope: &Rc<RefCell<Scope>>) -> EvalResult<JsValue> {
        match e {
            Expr::Num(n) => Ok(JsValue::Num(*n)),
            Expr::Str(s) => Ok(JsValue::Str(s.clone())),
            Expr::Bool(b) => Ok(JsValue::Bool(*b)),
            Expr::Null => Ok(JsValue::Null),
            Expr::Undefined => Ok(JsValue::Undefined),
            Expr::This => Ok(env::get(scope, "this").unwrap_or(JsValue::Undefined)),
            Expr::Ident(name) => {
                env::get(scope, name).ok_or_else(|| self.reference_error(name))
            }
            Expr::Template { strings, exprs } => {
                let mut s = String::new();
                for (i, q) in strings.iter().enumerate() {
                    s.push_str(q);
                    if let Some(e) = exprs.get(i) {
                        s.push_str(&self.eval(e, scope)?.to_js_string());
                    }
                }
                Ok(JsValue::str(s))
            }
            Expr::Array(elems) => {
                let mut items = Vec::with_capacity(elems.len());
                for el in elems {
                    match el {
                        Some(Expr::Spread(inner)) => {
                            let val = self.eval(inner, scope)?;
                            items.extend(self.iterable_values(&val)?);
                        }
                        Some(e) => items.push(self.eval(e, scope)?),
                        None => items.push(JsValue::Undefined),
                    }
                }
                Ok(JsValue::Object(Object::array(items)))
            }
            Expr::Object(props) => {
                let o = Object::plain();
                for p in props {
                    let Prop::KeyVal { key, value } = p;
                    let k = match key {
                        PropKey::Static(s) => s.clone(),
                        PropKey::Computed(e) => self.eval(e, scope)?.to_js_string(),
                    };
                    let v = self.eval(value, scope)?;
                    o.borrow_mut().set(&k, v);
                }
                Ok(JsValue::Object(o))
            }
            Expr::Unary { op, arg } => self.unary(*op, arg, scope),
            Expr::Update { op, prefix, arg } => self.update(*op, *prefix, arg, scope),
            Expr::Binary { op, l, r } => {
                let lv = self.eval(l, scope)?;
                let rv = self.eval(r, scope)?;
                self.binop(*op, lv, rv)
            }
            Expr::Logical { op, l, r } => {
                let lv = self.eval(l, scope)?;
                match op {
                    LogicalOp::And => {
                        if lv.truthy() {
                            self.eval(r, scope)
                        } else {
                            Ok(lv)
                        }
                    }
                    LogicalOp::Or => {
                        if lv.truthy() {
                            Ok(lv)
                        } else {
                            self.eval(r, scope)
                        }
                    }
                    LogicalOp::Nullish => {
                        if matches!(lv, JsValue::Undefined | JsValue::Null) {
                            self.eval(r, scope)
                        } else {
                            Ok(lv)
                        }
                    }
                }
            }
            Expr::Cond { cond, then, els } => {
                if self.eval(cond, scope)?.truthy() {
                    self.eval(then, scope)
                } else {
                    self.eval(els, scope)
                }
            }
            Expr::Assign { op, target, value } => self.assign(*op, target, value, scope),
            Expr::Member { obj, prop, computed } => {
                let ov = self.eval(obj, scope)?;
                let key = self.member_key(prop, *computed, scope)?;
                self.get_member(&ov, &key)
            }
            Expr::Call { callee, args } => self.eval_call(callee, args, scope),
            Expr::Func(def) => Ok(JsValue::Object(Object::function(def.clone(), scope.clone()))),
            Expr::Seq(exprs) => {
                let mut last = JsValue::Undefined;
                for e in exprs {
                    last = self.eval(e, scope)?;
                }
                Ok(last)
            }
            Expr::Super => Err(self.super_err()),
            Expr::Spread(_) => Err(self.type_error("unexpected spread")),
        }
    }

    fn unary(&self, op: UnOp, arg: &Expr, scope: &Rc<RefCell<Scope>>) -> EvalResult<JsValue> {
        // `typeof undeclared` must NOT throw — handle the bare-identifier case.
        if let (UnOp::TypeOf, Expr::Ident(name)) = (op, arg) {
            return Ok(JsValue::str(
                env::get(scope, name).map(|v| v.type_of()).unwrap_or("undefined"),
            ));
        }
        let v = self.eval(arg, scope)?;
        Ok(match op {
            UnOp::Neg => JsValue::Num(-v.to_number()),
            UnOp::Plus => JsValue::Num(v.to_number()),
            UnOp::Not => JsValue::Bool(!v.truthy()),
            UnOp::BitNot => JsValue::Num(!to_int32(&v) as f64),
            UnOp::TypeOf => JsValue::str(v.type_of()),
            UnOp::Void => JsValue::Undefined,
        })
    }

    fn update(&self, op: UpdateOp, prefix: bool, arg: &Expr, scope: &Rc<RefCell<Scope>>) -> EvalResult<JsValue> {
        let old = self.eval(arg, scope)?.to_number();
        let new = match op {
            UpdateOp::Inc => old + 1.0,
            UpdateOp::Dec => old - 1.0,
        };
        self.store(arg, JsValue::Num(new), scope)?;
        Ok(JsValue::Num(if prefix { new } else { old }))
    }

    fn assign(&self, op: Option<BinOp>, target: &Expr, value: &Expr, scope: &Rc<RefCell<Scope>>) -> EvalResult<JsValue> {
        let v = match op {
            None => self.eval(value, scope)?,
            Some(binop) => {
                let cur = self.eval(target, scope)?;
                let rhs = self.eval(value, scope)?;
                self.binop(binop, cur, rhs)?
            }
        };
        self.store(target, v.clone(), scope)?;
        Ok(v)
    }

    /// Store `v` into an assignment target (an identifier or a member access).
    fn store(&self, target: &Expr, v: JsValue, scope: &Rc<RefCell<Scope>>) -> EvalResult<()> {
        match target {
            Expr::Ident(name) => {
                if !env::set(scope, name, v.clone()) {
                    // implicit global (sloppy mode)
                    self.global.borrow_mut().declare(name, v);
                }
                Ok(())
            }
            Expr::Member { obj, prop, computed } => {
                let ov = self.eval(obj, scope)?;
                let key = self.member_key(prop, *computed, scope)?;
                self.set_member(&ov, &key, v)
            }
            _ => Err(self.type_error("invalid assignment target")),
        }
    }

    /// The values a `for-of`/spread iterates: array elements, or string chars.
    /// (The full iterator protocol / generators are a later tier.)
    fn iterable_values(&self, v: &JsValue) -> EvalResult<Vec<JsValue>> {
        match v {
            JsValue::Object(o) => {
                if let ObjData::Array(items) = &o.borrow().data {
                    Ok(items.clone())
                } else {
                    Err(self.type_error("value is not iterable"))
                }
            }
            JsValue::Str(s) => Ok(s.chars().map(|c| JsValue::str(c.to_string())).collect()),
            _ => Err(self.type_error(&format!("{} is not iterable", v.to_js_string()))),
        }
    }

    /// The own enumerable keys a `for-in` iterates.
    fn enum_keys(&self, v: &JsValue) -> Vec<String> {
        match v {
            JsValue::Object(o) => {
                let b = o.borrow();
                match &b.data {
                    ObjData::Array(items) => (0..items.len()).map(|i| i.to_string()).collect(),
                    _ => b.order.clone(),
                }
            }
            _ => Vec::new(),
        }
    }

    /// Bind a `for-of`/`for-in` loop variable: a fresh per-iteration binding for
    /// `let`/`const`/`var` (so closures capture distinct values), else an
    /// assignment to an existing variable.
    fn bind_loop_var(
        &self,
        decl: &Option<DeclKind>,
        name: &str,
        v: JsValue,
        outer: &Rc<RefCell<Scope>>,
        iter_scope: &Rc<RefCell<Scope>>,
    ) {
        if decl.is_some() {
            iter_scope.borrow_mut().declare(name, v);
        } else if !env::set(outer, name, v.clone()) {
            self.global.borrow_mut().declare(name, v);
        }
    }

    fn member_key(&self, prop: &Expr, computed: bool, scope: &Rc<RefCell<Scope>>) -> EvalResult<String> {
        if !computed {
            if let Expr::Str(s) = prop {
                return Ok(s.to_string());
            }
        }
        Ok(self.eval(prop, scope)?.to_js_string())
    }

    fn eval_args(&self, args: &[Expr], scope: &Rc<RefCell<Scope>>) -> EvalResult<Vec<JsValue>> {
        let mut v = Vec::with_capacity(args.len());
        for a in args {
            if let Expr::Spread(inner) = a {
                let val = self.eval(inner, scope)?;
                v.extend(self.iterable_values(&val)?);
            } else {
                v.push(self.eval(a, scope)?);
            }
        }
        Ok(v)
    }

    fn eval_call(&self, callee: &Expr, args: &[Expr], scope: &Rc<RefCell<Scope>>) -> EvalResult<JsValue> {
        // `super(...)` — invoke the parent constructor with the current `this`.
        if matches!(callee, Expr::Super) {
            let sup = env::get(scope, "%super%").ok_or_else(|| self.super_err())?;
            let this = env::get(scope, "this").unwrap_or(JsValue::Undefined);
            let argv = self.eval_args(args, scope)?;
            self.call(&sup, &this, &argv)?;
            return Ok(JsValue::Undefined);
        }
        // `super.m(...)` — invoke a parent prototype method with the current `this`.
        if let Expr::Member { obj, prop, computed } = callee {
            if matches!(obj.as_ref(), Expr::Super) {
                let sup = env::get(scope, "%super%").ok_or_else(|| self.super_err())?;
                let sproto = self.get_member(&sup, "prototype")?;
                let key = self.member_key(prop, *computed, scope)?;
                let m = self.get_member(&sproto, &key)?;
                let this = env::get(scope, "this").unwrap_or(JsValue::Undefined);
                let argv = self.eval_args(args, scope)?;
                return self.call(&m, &this, &argv);
            }
        }
        // A method call `obj.m(args)` binds `this = obj`.
        let (func, this) = match callee {
            Expr::Member { obj, prop, computed } => {
                let ov = self.eval(obj, scope)?;
                let key = self.member_key(prop, *computed, scope)?;
                let f = self.get_member(&ov, &key)?;
                (f, ov)
            }
            _ => (self.eval(callee, scope)?, JsValue::Undefined),
        };
        let argv = self.eval_args(args, scope)?;
        self.call(&func, &this, &argv)
    }

    fn super_err(&self) -> JsValue {
        self.make_error("SyntaxError", "'super' keyword unexpected here")
    }

    /// Call a function value with `this` and arguments.
    pub fn call(&self, func: &JsValue, this: &JsValue, args: &[JsValue]) -> EvalResult<JsValue> {
        let JsValue::Object(o) = func else {
            return Err(self.type_error(&format!("{} is not a function", func.display())));
        };
        // Extract what we need, then drop the borrow before running the body
        // (which may re-borrow the same object, e.g. recursion).
        enum Kind {
            Native(NativeFn),
            User(Rc<FuncDef>, Rc<RefCell<Scope>>),
        }
        let kind = {
            let b = o.borrow();
            match &b.data {
                ObjData::Native { f, .. } => Kind::Native(*f),
                ObjData::Function { def, scope } => Kind::User(def.clone(), scope.clone()),
                _ => return Err(self.type_error("value is not a function")),
            }
        };
        match kind {
            Kind::Native(f) => f(self, this, args),
            Kind::User(def, closure) => {
                let act = Scope::child(&closure);
                if !def.is_arrow {
                    act.borrow_mut().declare("this", this.clone());
                    act.borrow_mut().declare("arguments", JsValue::Object(Object::array(args.to_vec())));
                }
                for (i, p) in def.params.iter().enumerate() {
                    // A default value applies when the argument is missing OR
                    // explicitly `undefined`; defaults see earlier params.
                    let v = match args.get(i) {
                        Some(a) if !matches!(a, JsValue::Undefined) => a.clone(),
                        _ => match &p.default {
                            Some(d) => self.eval(d, &act)?,
                            None => JsValue::Undefined,
                        },
                    };
                    act.borrow_mut().declare(&p.name, v);
                }
                if let Some(rest) = &def.rest {
                    let extra: Vec<JsValue> = args.iter().skip(def.params.len()).cloned().collect();
                    act.borrow_mut().declare(rest, JsValue::Object(Object::array(extra)));
                }
                self.hoist(&def.body, &act);
                match self.exec_block(&def.body, &act)? {
                    Flow::Return(v) => Ok(v),
                    _ => Ok(JsValue::Undefined),
                }
            }
        }
    }

    // ───────────────────────── member access ─────────────────────────

    pub fn get_member(&self, obj: &JsValue, key: &str) -> EvalResult<JsValue> {
        match obj {
            JsValue::Object(o) => {
                {
                    let b = o.borrow();
                    if let ObjData::Array(items) = &b.data {
                        if key == "length" {
                            return Ok(JsValue::Num(items.len() as f64));
                        }
                        if let Ok(i) = key.parse::<usize>() {
                            return Ok(items.get(i).cloned().unwrap_or(JsValue::Undefined));
                        }
                        if let Some(v) = b.props.get(key) {
                            return Ok(v.clone());
                        }
                        if let Some(f) = crate::methods::array_method(key) {
                            return Ok(native(key, f));
                        }
                        return Ok(JsValue::Undefined);
                    }
                    if let Some(v) = b.props.get(key) {
                        return Ok(v.clone()); // own property
                    }
                }
                // walk the prototype chain
                let mut cur = o.borrow().proto.clone();
                while let Some(p) = cur {
                    if let Some(v) = p.borrow().props.get(key) {
                        return Ok(v.clone());
                    }
                    cur = p.borrow().proto.clone();
                }
                // a function auto-vivifies its `.prototype` object on first access
                // (so `C.prototype.m = …` and `new C()` work)
                let is_fn = matches!(o.borrow().data, ObjData::Function { .. } | ObjData::Native { .. });
                if is_fn && key == "prototype" {
                    let v = JsValue::Object(Object::plain());
                    o.borrow_mut().set(key, v.clone());
                    return Ok(v);
                }
                Ok(JsValue::Undefined)
            }
            JsValue::Str(s) => {
                if key == "length" {
                    return Ok(JsValue::Num(s.chars().count() as f64));
                }
                if let Ok(i) = key.parse::<usize>() {
                    return Ok(s
                        .chars()
                        .nth(i)
                        .map(|c| JsValue::str(c.to_string()))
                        .unwrap_or(JsValue::Undefined));
                }
                if let Some(f) = crate::methods::string_method(key) {
                    return Ok(native(key, f));
                }
                Ok(JsValue::Undefined)
            }
            JsValue::Num(_) => Ok(crate::methods::number_method(key).map(|f| native(key, f)).unwrap_or(JsValue::Undefined)),
            JsValue::Undefined | JsValue::Null => Err(self.type_error(&format!(
                "Cannot read properties of {} (reading '{key}')",
                obj.to_js_string()
            ))),
            JsValue::Bool(_) => Ok(JsValue::Undefined),
        }
    }

    pub fn set_member(&self, obj: &JsValue, key: &str, v: JsValue) -> EvalResult<()> {
        let JsValue::Object(o) = obj else {
            return Ok(()); // writing a property on a primitive is a no-op in sloppy mode
        };
        {
            let mut b = o.borrow_mut();
            if let ObjData::Array(items) = &mut b.data {
                if key == "length" {
                    let n = v.to_number();
                    let n = if n.is_finite() && n >= 0.0 { n as usize } else { 0 };
                    items.resize(n, JsValue::Undefined);
                    return Ok(());
                }
                if let Ok(i) = key.parse::<usize>() {
                    if i >= items.len() {
                        items.resize(i + 1, JsValue::Undefined);
                    }
                    items[i] = v;
                    return Ok(());
                }
            }
        }
        o.borrow_mut().set(key, v);
        Ok(())
    }

    // ───────────────────────── operators ─────────────────────────

    fn binop(&self, op: BinOp, l: JsValue, r: JsValue) -> EvalResult<JsValue> {
        use BinOp::*;
        Ok(match op {
            Add => {
                let lp = to_primitive(&l);
                let rp = to_primitive(&r);
                if matches!(lp, JsValue::Str(_)) || matches!(rp, JsValue::Str(_)) {
                    JsValue::str(format!("{}{}", lp.to_js_string(), rp.to_js_string()))
                } else {
                    JsValue::Num(lp.to_number() + rp.to_number())
                }
            }
            Sub => JsValue::Num(l.to_number() - r.to_number()),
            Mul => JsValue::Num(l.to_number() * r.to_number()),
            Div => JsValue::Num(l.to_number() / r.to_number()),
            Mod => JsValue::Num(js_mod(l.to_number(), r.to_number())),
            Pow => JsValue::Num(l.to_number().powf(r.to_number())),
            StrictEq => JsValue::Bool(l.strict_eq(&r)),
            StrictNotEq => JsValue::Bool(!l.strict_eq(&r)),
            EqEq => JsValue::Bool(l.loose_eq(&r)),
            NotEq => JsValue::Bool(!l.loose_eq(&r)),
            Lt | Le | Gt | Ge => compare(op, &l, &r),
            BitAnd => JsValue::Num((to_int32(&l) & to_int32(&r)) as f64),
            BitOr => JsValue::Num((to_int32(&l) | to_int32(&r)) as f64),
            BitXor => JsValue::Num((to_int32(&l) ^ to_int32(&r)) as f64),
            Shl => JsValue::Num((to_int32(&l).wrapping_shl(to_uint32(&r) & 31)) as f64),
            Shr => JsValue::Num((to_int32(&l) >> (to_uint32(&r) & 31)) as f64),
            UShr => JsValue::Num((to_uint32(&l) >> (to_uint32(&r) & 31)) as f64),
        })
    }

    // ───────────────────────── errors ─────────────────────────

    pub fn type_error(&self, msg: &str) -> JsValue {
        self.make_error("TypeError", msg)
    }
    pub fn reference_error(&self, name: &str) -> JsValue {
        self.make_error("ReferenceError", &format!("{name} is not defined"))
    }
    pub fn range_error(&self, msg: &str) -> JsValue {
        self.make_error("RangeError", msg)
    }

    /// Build an `Error`-like object `{ name, message }` (the prototype chain +
    /// real `Error` constructors come in a later tier).
    pub fn make_error(&self, name: &str, msg: &str) -> JsValue {
        let o = Object::plain();
        {
            let mut b = o.borrow_mut();
            b.set("name", JsValue::str(name));
            b.set("message", JsValue::str(msg));
            b.set("stack", JsValue::str(format!("{name}: {msg}")));
        }
        JsValue::Object(o)
    }
}

fn native(name: &str, f: NativeFn) -> JsValue {
    JsValue::Object(Object::native(name, f))
}

// ───────────────────────── numeric / coercion helpers ─────────────────────────

/// ToPrimitive(default) for the `+` operator — arrays/objects via their string
/// form (no user `valueOf`/`toString` dispatch yet).
fn to_primitive(v: &JsValue) -> JsValue {
    match v {
        JsValue::Object(_) => JsValue::str(v.to_js_string()),
        other => other.clone(),
    }
}

/// JS `%` (truncated remainder, sign of the dividend; NaN on 0 divisor).
fn js_mod(a: f64, b: f64) -> f64 {
    if b == 0.0 || a.is_nan() || b.is_nan() || a.is_infinite() {
        f64::NAN
    } else if b.is_infinite() {
        a
    } else {
        a % b
    }
}

/// Relational comparison (`<` `<=` `>` `>=`): lexicographic for two strings,
/// numeric otherwise; any NaN operand yields `false`.
fn compare(op: BinOp, l: &JsValue, r: &JsValue) -> JsValue {
    use BinOp::*;
    if let (JsValue::Str(a), JsValue::Str(b)) = (l, r) {
        return JsValue::Bool(match op {
            Lt => a < b,
            Le => a <= b,
            Gt => a > b,
            Ge => a >= b,
            _ => unreachable!(),
        });
    }
    let (a, b) = (l.to_number(), r.to_number());
    if a.is_nan() || b.is_nan() {
        return JsValue::Bool(false);
    }
    JsValue::Bool(match op {
        Lt => a < b,
        Le => a <= b,
        Gt => a > b,
        Ge => a >= b,
        _ => unreachable!(),
    })
}

/// ECMAScript ToInt32.
pub fn to_int32(v: &JsValue) -> i32 {
    to_uint32(v) as i32
}

/// ECMAScript ToUint32.
pub fn to_uint32(v: &JsValue) -> u32 {
    let n = v.to_number();
    if !n.is_finite() || n == 0.0 {
        return 0;
    }
    let m = n.trunc().rem_euclid(4294967296.0); // 2^32
    m as u32
}
