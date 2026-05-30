//! The bytecode VM: executes a [`Chunk`] over a flat locals array and an operand
//! stack, with no per-call scope map and no variable-name hashing for locals —
//! that's the win over the tree-walker. Free variables (globals / captured) are
//! resolved through the function's closure scope, exactly as the tree-walker
//! does, so semantics match. Anything the compiler couldn't handle never reaches
//! here (those functions stay on the tree-walker).
//!
//! The `locals` and operand `stack` buffers are recycled through a per-interp
//! freelist ([`Interp::buf_pool`]) so a steady-state call (e.g. deep recursion)
//! does ZERO heap allocation per frame — the buffers are taken on entry and
//! returned on exit.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::ast::FuncDef;
use crate::bytecode::{Chunk, Op};
use crate::compile::compile_fn;
use crate::env::{self, Scope};
use crate::interp::{EvalResult, Interp};
use crate::value::JsValue;

/// Per-interpreter cache of compiled chunks, keyed by FuncDef identity.
/// `None` = compilation was attempted and the function is unsupported (stay on
/// the tree-walker); `Some` = a compiled chunk to run.
pub type CompileCache = RefCell<HashMap<usize, Option<Rc<Chunk>>>>;

/// A freelist of reusable value buffers (for VM frames' locals + operand stacks).
pub type BufPool = RefCell<Vec<Vec<JsValue>>>;

impl Interp {
    /// Look up (or compute and memoize) the compiled chunk for `def`. Returns
    /// `None` if the function is outside the compilable subset.
    pub(crate) fn get_chunk(&self, def: &Rc<FuncDef>) -> Option<Rc<Chunk>> {
        let key = Rc::as_ptr(def) as usize;
        if let Some(entry) = self.compiled.borrow().get(&key) {
            return entry.clone();
        }
        let compiled = compile_fn(def).ok().map(Rc::new);
        self.compiled.borrow_mut().insert(key, compiled.clone());
        compiled
    }

    #[inline]
    fn take_buf(&self) -> Vec<JsValue> {
        self.buf_pool.borrow_mut().pop().unwrap_or_default()
    }
    #[inline]
    fn give_buf(&self, mut b: Vec<JsValue>) {
        b.clear();
        // Cap retained capacity so a one-off huge frame doesn't pin memory.
        let mut pool = self.buf_pool.borrow_mut();
        if pool.len() < 256 {
            pool.push(b);
        }
    }

    /// Run a compiled `chunk`: `closure` is the function's captured scope (for
    /// free-variable resolution), `args` fills the leading local slots. Buffers
    /// come from the pool and are returned on every exit path.
    pub(crate) fn run_chunk(
        &self,
        chunk: &Chunk,
        closure: &Rc<RefCell<Scope>>,
        args: &[JsValue],
    ) -> EvalResult<JsValue> {
        let mut locals = self.take_buf();
        locals.resize(chunk.nlocals, JsValue::Undefined);
        for i in 0..chunk.nparams.min(args.len()) {
            locals[i] = args[i].clone();
        }
        let mut stack = self.take_buf();
        let result = self.exec_chunk(chunk, closure, &mut locals, &mut stack);
        self.give_buf(locals);
        self.give_buf(stack);
        result
    }

    /// The instruction loop. Kept separate from buffer management so `run_chunk`
    /// can return the buffers to the pool no matter how this exits.
    fn exec_chunk(
        &self,
        chunk: &Chunk,
        closure: &Rc<RefCell<Scope>>,
        locals: &mut [JsValue],
        stack: &mut Vec<JsValue>,
    ) -> EvalResult<JsValue> {
        let mut pc = 0usize;
        let code = &chunk.code;
        while pc < code.len() {
            match &code[pc] {
                Op::PushConst(i) => stack.push(chunk.consts[*i as usize].clone()),
                Op::PushUndefined => stack.push(JsValue::Undefined),
                Op::PushNull => stack.push(JsValue::Null),
                Op::PushTrue => stack.push(JsValue::Bool(true)),
                Op::PushFalse => stack.push(JsValue::Bool(false)),
                Op::Pop => {
                    stack.pop();
                }
                Op::Dup => {
                    let v = stack.last().cloned().unwrap_or(JsValue::Undefined);
                    stack.push(v);
                }
                Op::LoadLocal(n) => stack.push(locals[*n as usize].clone()),
                Op::StoreLocal(n) => {
                    // peek: assignment yields the value
                    locals[*n as usize] = stack.last().cloned().unwrap_or(JsValue::Undefined);
                }
                Op::LoadName(i) => {
                    let name = &chunk.names[*i as usize];
                    match env::get(closure, name) {
                        Some(v) => stack.push(v),
                        None => return Err(self.reference_error(name)),
                    }
                }
                Op::StoreName(i) => {
                    let name = &chunk.names[*i as usize];
                    let v = stack.last().cloned().unwrap_or(JsValue::Undefined);
                    if !env::set(closure, name, v.clone()) {
                        self.global.borrow_mut().declare(name, v);
                    }
                }
                Op::Bin(op) => {
                    let r = stack.pop().unwrap_or(JsValue::Undefined);
                    let l = stack.pop().unwrap_or(JsValue::Undefined);
                    stack.push(self.binop(*op, l, r)?);
                }
                Op::Neg => {
                    let v = stack.pop().unwrap_or(JsValue::Undefined);
                    stack.push(JsValue::Num(-v.to_number()));
                }
                Op::Pos => {
                    let v = stack.pop().unwrap_or(JsValue::Undefined);
                    stack.push(JsValue::Num(v.to_number()));
                }
                Op::Not => {
                    let v = stack.pop().unwrap_or(JsValue::Undefined);
                    stack.push(JsValue::Bool(!v.truthy()));
                }
                Op::Jump(t) => {
                    pc = *t as usize;
                    continue;
                }
                Op::JumpIfFalse(t) => {
                    let v = stack.pop().unwrap_or(JsValue::Undefined);
                    if !v.truthy() {
                        pc = *t as usize;
                        continue;
                    }
                }
                Op::JumpIfTrue(t) => {
                    let v = stack.pop().unwrap_or(JsValue::Undefined);
                    if v.truthy() {
                        pc = *t as usize;
                        continue;
                    }
                }
                Op::Call(argc) => {
                    let argc = *argc as usize;
                    let at = stack.len() - argc;
                    let args: Vec<JsValue> = stack.split_off(at);
                    let callee = stack.pop().unwrap_or(JsValue::Undefined);
                    let r = self.call(&callee, &JsValue::Undefined, &args)?;
                    stack.push(r);
                }
                Op::Return => {
                    return Ok(stack.pop().unwrap_or(JsValue::Undefined));
                }
            }
            pc += 1;
        }
        Ok(JsValue::Undefined)
    }
}
