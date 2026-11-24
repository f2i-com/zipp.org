//! The Python runtime's generator step, native (`__zipp_py_gen`).
//!
//! A Python generator is a runtime record (`rt.makeGenerator` in
//! `runtime/core.js`) around an engine generator (`js`). Its `next` member
//! is what every consumer calls for one step: a `for` loop's header, the
//! runtime's `fornext`, `next()`, `list()`, `sum()` and the rest. In a
//! Python program that member is this native; it performs `genNext` (the
//! runtime's JavaScript step, kept as the fallback and for every other
//! build) for the one state that matters for speed, a plain resume, and
//! hands every other state to `genNext` itself:
//!
//! * finished: `returned = null`, the STOP sentinel (as `genNext`);
//! * running (re-entered from its own body), or holding exceptions it was
//!   handling when it last yielded (`excs`), or a record or engine value of
//!   an unexpected shape: `genNext` runs, and raises or restores exactly as
//!   it always did;
//! * a plain resume: `running`/`started` set, the engine generator resumed,
//!   and then exactly `genNext`'s bookkeeping: the current-exception stack
//!   cut back to its depth (what the body left pushed at a yield becomes
//!   `excs`), `done`/`returned` on completion. An exception escaping the
//!   body cuts the stack back and is converted by the runtime's own
//!   `genEscape` (PEP 479), whose result is thrown.
//!
//! Nothing here runs guest code except through the resumed body and the
//! runtime functions named above.

use super::*;
use crate::heap::HeapObj;
use crate::value::Value;

/// The record's fields this step reads or writes, with the slot each has in
/// the record `makeGenerator` builds (the literal's key order; an `id` a
/// later `id()` adds goes after them).
const F_JS: (usize, &str) = (1, "js");
const F_DONE: (usize, &str) = (2, "done");
const F_STARTED: (usize, &str) = (3, "started");
const F_RUNNING: (usize, &str) = (4, "running");
const F_RETURNED: (usize, &str) = (5, "returned");
const F_EXCS: (usize, &str) = (6, "excs");
const F_GRT: (usize, &str) = (13, "grt");

// The shape of a generator record whose fields above were all found at
// their usual slots, as data properties, is the VM's `PyRt::gen_shape`
// (`shape::DICT`, which never matches, until one is seen).

/// The runtime values the step needs (`grt`, one array shared by every
/// generator record): the JavaScript `genNext`, the current-exception
/// stack, the STOP sentinel and `genEscape`.
#[derive(Clone, Copy)]
pub(super) struct GenRuntime {
    gen_next: Value,
    exc_stack: u32,
    stop: Value,
    escape: Value,
}

impl<'p> Vm<'p> {
    /// The slot of the record's own data property `field` (its usual slot
    /// when the key is there, else a lookup).
    fn py_gen_slot(&self, idx: u32, field: (usize, &str)) -> Option<usize> {
        let HeapObj::Object(m) = self.heap.get(idx) else {
            return None;
        };
        let slot = if field.0 < m.slot_len() && m.key_at(field.0) == field.1 {
            field.0
        } else {
            m.pos(field.1)?
        };
        (!m.attr_at(slot).accessor).then_some(slot)
    }

    #[inline]
    fn py_gen_read(&self, idx: u32, slot: usize) -> Value {
        match self.heap.get(idx) {
            HeapObj::Object(m) => m.val_at(slot),
            _ => Value::UNDEFINED,
        }
    }

    #[inline]
    fn py_gen_write(&mut self, idx: u32, slot: usize, v: Value) {
        if v.is_heap() {
            self.heap.write_barrier_val(idx, v);
        }
        if let HeapObj::Object(m) = self.heap.get_mut(idx) {
            m.set_val_at(slot, v);
        }
    }

    fn py_gen_runtime(&self, grt: Value) -> Option<GenRuntime> {
        if !grt.is_heap() {
            return None;
        }
        let HeapObj::Array(items) = self.heap.get(grt.heap_index()) else {
            return None;
        };
        let [gen_next, exc_stack, stop, escape] = items.get(..4)? else {
            return None;
        };
        if !exc_stack.is_heap() || !matches!(self.heap.get(exc_stack.heap_index()), HeapObj::Array(_)) {
            return None;
        }
        Some(GenRuntime {
            gen_next: *gen_next,
            exc_stack: exc_stack.heap_index(),
            stop: *stop,
            escape: *escape,
        })
    }

    #[inline]
    fn py_exc_depth(&self, stack: u32) -> Option<usize> {
        match self.heap.get(stack) {
            HeapObj::Array(items) => Some(items.len()),
            _ => None,
        }
    }

    /// The runtime array `grt` of a record of the known shape: the one
    /// validated last (kept in `pins`, so its slot is never another array's
    /// while the registry lives), or validated now and kept.
    #[inline]
    fn py_gen_runtime_cached(&mut self, grt: Value) -> Option<GenRuntime> {
        let p = self.py_rt.as_deref()?;
        if let Some((bits, rt)) = p.gen_rt.get() {
            if bits == grt.bits() {
                return Some(rt);
            }
        }
        let rt = self.py_gen_runtime(grt)?;
        let p = self.py_rt.as_deref()?;
        let pins = p.pins;
        p.gen_rt.set(Some((grt.bits(), rt)));
        if pins.is_heap() {
            let pi = pins.heap_index();
            self.heap.write_barrier_val(pi, grt);
            if let HeapObj::Array(a) = self.heap.get_mut(pi) {
                a.push(grt);
            }
        }
        Some(rt)
    }

    /// Native zero-allocation step for a plain `T.list_iterator` record
    /// (`{ cls: T.list_iterator, seq, i, next }`). Never allocates, never runs
    /// guest code, and keeps exhausted iterators exhausted (`i = -1`).
    #[inline]
    fn py_list_iter_step(&mut self, this: Value, learn_shape: bool) -> Option<Value> {
        if !this.is_heap() {
            return None;
        }
        let p = self.py_rt.as_deref()?;
        let t_list_iter = p.t_list_iter;
        let stop = p.stop;
        if !t_list_iter.is_heap() || stop.is_undefined() {
            return None;
        }
        let shape = p.list_iter_shape.get();
        let seq_shape = p.seq_shape;
        let g = this.heap_index();
        let (seq, idx_v) = match self.heap.get(g) {
            HeapObj::Object(m) if !m.is_ctor && shape != crate::shape::DICT && m.shape() == shape => {
                if m.val_at(0).bits() != t_list_iter.bits() {
                    return None;
                }
                (m.val_at(1), m.val_at(2))
            }
            HeapObj::Object(m)
                if learn_shape
                    && !m.is_ctor
                    && m.slot_len() >= 4
                    && m.val_at(0).bits() == t_list_iter.bits()
                    && super::py_rt::key_eq(m.key_at(0), "cls")
                    && super::py_rt::key_eq(m.key_at(1), "seq")
                    && super::py_rt::key_eq(m.key_at(2), "i")
                    && super::py_rt::key_eq(m.key_at(3), "next")
                    && !m.attr_at(0).accessor
                    && !m.attr_at(1).accessor
                    && !m.attr_at(2).accessor
                    && !m.attr_at(3).accessor =>
            {
                if m.shape_guardable() {
                    p.list_iter_shape.set(m.shape());
                }
                (m.val_at(1), m.val_at(2))
            }
            _ => return None,
        };
        if !idx_v.is_int() {
            return None;
        }
        let i = idx_v.as_int();
        if i < 0 {
            return Some(stop);
        }
        if !seq.is_heap() {
            return None;
        }
        let items = match self.heap.get(seq.heap_index()) {
            HeapObj::Object(sm) if !sm.is_ctor => {
                if seq_shape != crate::shape::DICT && sm.shape() == seq_shape {
                    sm.val_at(1)
                } else if sm.slot_len() > 1
                    && super::py_rt::key_eq(sm.key_at(1), "items")
                    && !sm.attr_at(1).accessor
                {
                    sm.val_at(1)
                } else if let Some(s) = sm.pos("items") {
                    if sm.attr_at(s).accessor {
                        return None;
                    }
                    sm.val_at(s)
                } else {
                    return None;
                }
            }
            _ => return None,
        };
        if !items.is_heap() {
            return None;
        }
        let items_idx = items.heap_index();
        if (!self.array_js_len.is_empty() && self.array_js_len.contains_key(&items_idx))
            || (!self.arr_props.is_empty() && self.arr_props.contains_key(&items_idx))
        {
            return None;
        }
        let (out, next_i) = match self.heap.get(items_idx) {
            HeapObj::Array(arr) => {
                let ui = i as usize;
                if ui < arr.len() {
                    let v = arr[ui];
                    if v.is_hole() || i == i32::MAX {
                        return None;
                    }
                    (v, i + 1)
                } else {
                    (stop, -1)
                }
            }
            _ => return None,
        };
        if let HeapObj::Object(m) = self.heap.get_mut(g) {
            m.set_val_at(2, Value::int(next_i));
        }
        Some(out)
    }

    /// Ultra-fast pure in-register mini-stepper for an already-started,
    /// suspended plain generator (`GenState::Suspended(resume_ip)` at a plain
    /// `Instr::Yield`, empty handlers, no eval scope, no `arguments`).
    ///
    /// Executes up to 16 pure, non-allocating bytecode ops directly against
    /// `GeneratorData::regs` using a stack journal, committing atomically only
    /// upon reaching the next `Instr::Yield` or `Instr::Return`. Never pushes
    /// a `Frame`, never copies `regs`, never allocates on the VM heap, and
    /// never invokes guest code — so `jit_py_gen_next` can call it before
    /// `maybe_gc()` and before bumping `jit_call_depth`. Returns `None` with
    /// zero mutations for anything else.
    #[inline(always)]
    pub(crate) fn py_gen_next_fast(&mut self, this: Value) -> Option<Value> {
        use crate::bytecode::Instr;
        use crate::heap::GenState;

        if super::prof_py::on()
            || (self.instr_rec.is_some() && !self.native_kernel_admits(32, 0))
            || !this.is_heap()
        {
            return None;
        }
        if let Some(v) = self.py_list_iter_step(this, false) {
            return Some(v);
        }
        let g = this.heap_index();
        let shape = self.py_rt.as_deref()?.gen_shape.get();
        let (js, grt) = match self.heap.get(g) {
            HeapObj::Object(m) if !m.is_ctor && shape != crate::shape::DICT && m.shape() == shape => {
                let done = m.val_at(F_DONE.0);
                let started = m.val_at(F_STARTED.0);
                let running = m.val_at(F_RUNNING.0);
                let excs = m.val_at(F_EXCS.0);
                let js = m.val_at(F_JS.0);
                if done != Value::FALSE
                    || started != Value::TRUE
                    || running != Value::FALSE
                    || excs != Value::NULL
                    || !js.is_heap()
                {
                    return None;
                }
                (js, m.val_at(F_GRT.0))
            }
            _ => return None,
        };
        let js_idx = js.heap_index();
        if !self.closure_eval_scope.is_empty() && self.closure_eval_scope.contains_key(&js_idx) {
            return None;
        }

        // Fast counted-loop shortcut before allocating the undo buffer or
        // entering the general bytecode stepper: covers `for i in range(n): yield i`
        // (direct back-edge or 1/2-hop `JumpIfTrue` mode dispatch) with zero
        // stack undo buffer and zero loop dispatch.
        {
            let HeapObj::Generator(gen) = self.heap.get_mut(js_idx) else {
                return None;
            };
            let GenState::Suspended(resume_ip) = gen.state else {
                return None;
            };
            if !gen.handlers.is_empty() {
                return None;
            }
            let fid = gen.func as usize;
            let proto = if fid < self.main_func_count {
                &self.program.functions[fid]
            } else {
                &self.eval_funcs[fid - self.main_func_count]
            };
            if proto.arguments_reg.is_some() {
                return None;
            }
            let code = proto.code.as_slice();
            let regs = gen.regs.as_mut_slice();
            let (yield_dst, yield_val) = match code.get(resume_ip) {
                Some(&Instr::Yield { dst, val }) if (dst as usize) < regs.len() => (dst, val),
                _ => return None,
            };
            if resume_ip >= 3 {
                if let (
                    Some(&Instr::JumpIfNotLt { a: cur, b: end, .. }),
                    Some(&Instr::Move { dst: yv2, src: cur2 }),
                    Some(&Instr::AddInt { dst: cur3, a: cur4, imm, upd: true }),
                    Some(&Instr::Jump { target }),
                ) = (
                    code.get(resume_ip - 3),
                    code.get(resume_ip - 2),
                    code.get(resume_ip - 1),
                    code.get(resume_ip + 1),
                ) {
                    let head_start = resume_ip - 3;
                    let t = target as usize;
                    let hop_ok: Option<(i64, u16)> = if t == head_start {
                        Some((5, u16::MAX))
                    } else if let Some(&Instr::JumpIfTrue { cond: fast, target: ct }) = code.get(t) {
                        if ct as usize == head_start
                            && (fast as usize) < regs.len()
                            && regs[fast as usize] == Value::TRUE
                        {
                            Some((6, fast))
                        } else if (fast as usize) < regs.len() && regs[fast as usize] == Value::FALSE {
                            if let Some(&Instr::JumpIfTrue { cond: fast2, target: ct2 }) = code.get(t + 1) {
                                if ct2 as usize == head_start
                                    && (fast2 as usize) < regs.len()
                                    && regs[fast2 as usize] == Value::TRUE
                                {
                                    Some((7, fast2))
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    if let Some((step_cost, fast_reg)) = hop_ok {
                        if yv2 == yield_val
                            && cur2 == cur
                            && cur3 == cur
                            && cur4 == cur
                            && cur != end
                            && yield_val != cur
                            && yield_val != end
                            && yield_dst != cur
                            && yield_dst != end
                            && yield_dst != yield_val
                            && fast_reg != cur
                            && fast_reg != yield_val
                            && fast_reg != yield_dst
                            && (cur as usize) < regs.len()
                            && (end as usize) < regs.len()
                            && (yield_val as usize) < regs.len()
                        {
                            let cv = regs[cur as usize];
                            let ev = regs[end as usize];
                            if let (Some(x), Some(lim)) = (cv.small_bigint_val(), ev.small_bigint_val()) {
                                if x < lim {
                                    if let Some(next_v) = Value::small_bigint((x as i128) + (imm as i128)) {
                                        regs[yield_dst as usize] = Value::UNDEFINED;
                                        regs[yield_val as usize] = cv;
                                        regs[cur as usize] = next_v;
                                        if self.instr_rec.is_some() {
                                            self.charge_steps(step_cost);
                                        }
                                        return Some(cv);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        self.py_gen_next_fast_general(g, js_idx, grt)
    }

    #[inline(never)]
    fn py_gen_next_fast_general(&mut self, g: u32, js_idx: u32, grt: Value) -> Option<Value> {
        use super::py_ops::py_small_arith;
        use crate::bytecode::{Instr, PyArithOp};
        use crate::heap::GenState;

        const MAX_UNDO: usize = 4;
        let mut undo_r = [0u16; MAX_UNDO];
        let mut undo_v = [Value::UNDEFINED; MAX_UNDO];
        let mut n_undo: usize;
        let mut heap_written = false;

        enum GenStepDone {
            Yield(Value),
            Return(Value),
            Bail,
        }

        let mut steps = 0i64;
        let outcome = {
            let HeapObj::Generator(gen) = self.heap.get_mut(js_idx) else {
                return None;
            };
            let GenState::Suspended(resume_ip) = gen.state else {
                return None;
            };
            let fid = gen.func as usize;
            let proto = if fid < self.main_func_count {
                &self.program.functions[fid]
            } else {
                &self.eval_funcs[fid - self.main_func_count]
            };
            let code = proto.code.as_slice();
            let regs = gen.regs.as_mut_slice();
            let (yield_dst, _yield_val) = match code.get(resume_ip) {
                Some(&Instr::Yield { dst, val }) if (dst as usize) < regs.len() => (dst, val),
                _ => return None,
            };

            macro_rules! bail_in_loop {
                () => {{
                    for k in (0..n_undo).rev() {
                        regs[undo_r[k] as usize] = undo_v[k];
                    }
                    break GenStepDone::Bail;
                }};
            }
            macro_rules! set_r {
                ($r:expr, $val:expr) => {{
                    let rv: Value = $val;
                    if rv.is_heap() {
                        heap_written = true;
                    }
                    let ri = $r as usize;
                    if n_undo >= MAX_UNDO || ri >= regs.len() {
                        bail_in_loop!();
                    }
                    undo_r[n_undo] = $r;
                    undo_v[n_undo] = regs[ri];
                    n_undo += 1;
                    regs[ri] = rv;
                }};
            }

            #[inline(always)]
            fn cmp_fast(op: crate::bytecode::PyCmpOp, va: Value, vb: Value) -> Option<bool> {
                use crate::bytecode::PyCmpOp as C;
                if let (Some(x), Some(y)) = (va.small_bigint_val(), vb.small_bigint_val()) {
                    return Some(match op {
                        C::Lt => x < y,
                        C::Le => x <= y,
                        C::Gt => x > y,
                        C::Ge => x >= y,
                        C::Eq => x == y,
                        C::Ne => x != y,
                    });
                }
                if va.is_number() && vb.is_number() {
                    let (x, y) = (va.as_f64(), vb.as_f64());
                    return Some(match op {
                        C::Lt => x < y,
                        C::Le => x <= y,
                        C::Gt => x > y,
                        C::Ge => x >= y,
                        C::Eq => x == y,
                        C::Ne => x != y,
                    });
                }
                None
            }

            if regs[yield_dst as usize] != Value::UNDEFINED {
                undo_r[0] = yield_dst;
                undo_v[0] = regs[yield_dst as usize];
                n_undo = 1;
                regs[yield_dst as usize] = Value::UNDEFINED;
            } else {
                n_undo = 0;
            }

            let mut ip = resume_ip + 1;
            loop {
                if steps >= 16 {
                    bail_in_loop!();
                }
                let Some(&instr) = code.get(ip) else {
                    bail_in_loop!();
                };
                steps += 1;
                match instr {
                    Instr::Move { dst, src } => {
                        let v = regs[src as usize];
                        set_r!(dst, v);
                        ip += 1;
                    }
                    Instr::LoadNull { dst } => {
                        set_r!(dst, Value::NULL);
                        ip += 1;
                    }
                    Instr::LoadUndefined { dst } => {
                        set_r!(dst, Value::UNDEFINED);
                        ip += 1;
                    }
                    Instr::LoadBool { dst, val } => {
                        set_r!(dst, Value::bool(val));
                        ip += 1;
                    }
                    Instr::LoadInt { dst, val } => {
                        set_r!(dst, Value::int(val));
                        ip += 1;
                    }
                    Instr::LoadBigInt { dst, value } => {
                        let Some(sb) = Value::small_bigint(value.get()) else {
                            bail_in_loop!();
                        };
                        set_r!(dst, sb);
                        ip += 1;
                    }
                    Instr::Not { dst, a } => {
                        let v = regs[a as usize];
                        let out = if v == Value::TRUE {
                            Value::FALSE
                        } else if v == Value::FALSE {
                            Value::TRUE
                        } else {
                            bail_in_loop!();
                        };
                        set_r!(dst, out);
                        ip += 1;
                    }
                    Instr::AddInt { dst, a, imm, upd } => {
                        let va = regs[a as usize];
                        let res = if va.is_int() {
                            match va.as_int().checked_add(imm) {
                                Some(v) => Value::int(v),
                                None => Value::num(va.as_int() as f64 + imm as f64),
                            }
                        } else if va.is_double() {
                            Value::num(va.as_f64() + imm as f64)
                        } else if upd {
                            let Some(x) = va.small_bigint_val() else {
                                bail_in_loop!();
                            };
                            let Some(sb) = Value::small_bigint((x as i128) + (imm as i128)) else {
                                bail_in_loop!();
                            };
                            sb
                        } else {
                            bail_in_loop!();
                        };
                        set_r!(dst, res);
                        ip += 1;
                    }
                    Instr::PyAddImm { dst, a, imm, .. } => {
                        let va = regs[a as usize];
                        let res = if let Some(x) = va.small_bigint_val() {
                            let Some(sb) = Value::small_bigint((x as i128) + (imm as i128)) else {
                                bail_in_loop!();
                            };
                            sb
                        } else if va.is_number() {
                            Value::num(va.as_f64() + imm as f64)
                        } else {
                            bail_in_loop!();
                        };
                        set_r!(dst, res);
                        ip += 1;
                    }
                    Instr::PyArith { op, dst, a, b, .. } => {
                        let (va, vb) = (regs[a as usize], regs[b as usize]);
                        let res = if let (Some(x), Some(y)) = (va.small_bigint_val(), vb.small_bigint_val()) {
                            let Some(sb) = py_small_arith(op, x, y).and_then(|r| Value::small_bigint(r as i128)) else {
                                bail_in_loop!();
                            };
                            sb
                        } else if va.is_number() && vb.is_number() {
                            match op {
                                PyArithOp::Add => Value::num(va.as_f64() + vb.as_f64()),
                                PyArithOp::Sub => Value::num(va.as_f64() - vb.as_f64()),
                                PyArithOp::Mul => Value::num(va.as_f64() * vb.as_f64()),
                                PyArithOp::TrueDiv => {
                                    let y = vb.as_f64();
                                    if y == 0.0 {
                                        bail_in_loop!();
                                    }
                                    Value::num(va.as_f64() / y)
                                }
                                _ => bail_in_loop!(),
                            }
                        } else {
                            bail_in_loop!();
                        };
                        set_r!(dst, res);
                        ip += 1;
                    }
                    Instr::PyCompare { op, dst, a, b, .. } => {
                        let Some(r) = cmp_fast(op, regs[a as usize], regs[b as usize]) else {
                            bail_in_loop!();
                        };
                        set_r!(dst, Value::bool(r));
                        ip += 1;
                    }
                    Instr::PyJumpCompare { op, a, b, when, target, .. } => {
                        let Some(r) = cmp_fast(op, regs[a as usize], regs[b as usize]) else {
                            bail_in_loop!();
                        };
                        if r == when {
                            ip = target as usize;
                        } else {
                            ip += 1;
                        }
                    }
                    Instr::Jump { target } => {
                        ip = target as usize;
                    }
                    Instr::JumpIfFalse { cond, target } => {
                        let v = regs[cond as usize];
                        if v == Value::FALSE {
                            ip = target as usize;
                        } else if v == Value::TRUE {
                            ip += 1;
                        } else {
                            bail_in_loop!();
                        }
                    }
                    Instr::JumpIfTrue { cond, target } => {
                        let v = regs[cond as usize];
                        if v == Value::TRUE {
                            ip = target as usize;
                        } else if v == Value::FALSE {
                            ip += 1;
                        } else {
                            bail_in_loop!();
                        }
                    }
                    Instr::JumpIfNotLt { a, b, target } => {
                        let (va, vb) = (regs[a as usize], regs[b as usize]);
                        let lt = if let (Some(x), Some(y)) = (va.small_bigint_val(), vb.small_bigint_val()) {
                            x < y
                        } else if va.is_number() && vb.is_number() {
                            va.as_f64() < vb.as_f64()
                        } else {
                            bail_in_loop!();
                        };
                        if !lt {
                            ip = target as usize;
                        } else {
                            ip += 1;
                        }
                    }
                    Instr::JumpIfNotLe { a, b, target } => {
                        let (va, vb) = (regs[a as usize], regs[b as usize]);
                        let le = if let (Some(x), Some(y)) = (va.small_bigint_val(), vb.small_bigint_val()) {
                            x <= y
                        } else if va.is_number() && vb.is_number() {
                            va.as_f64() <= vb.as_f64()
                        } else {
                            bail_in_loop!();
                        };
                        if !le {
                            ip = target as usize;
                        } else {
                            ip += 1;
                        }
                    }
                    Instr::Yield { val, .. } => {
                        let yielded = regs[val as usize];
                        gen.state = GenState::Suspended(ip);
                        break GenStepDone::Yield(yielded);
                    }
                    Instr::Return { src } => {
                        let ret = regs[src as usize];
                        break GenStepDone::Return(ret);
                    }
                    _ => bail_in_loop!(),
                }
            }
        };

        match outcome {
            GenStepDone::Yield(yielded) => {
                if heap_written {
                    for k in 0..n_undo {
                        let v = match self.heap.get(js_idx) {
                            HeapObj::Generator(gen) => gen.regs[undo_r[k] as usize],
                            _ => Value::UNDEFINED,
                        };
                        if v.is_heap() {
                            self.heap.write_barrier_val(js_idx, v);
                        }
                    }
                }
                if self.instr_rec.is_some() {
                    self.charge_steps(steps);
                }
                Some(yielded)
            }
            GenStepDone::Return(ret) => {
                let Some(rt) = self.py_gen_runtime_cached(grt) else {
                    if let HeapObj::Generator(gen) = self.heap.get_mut(js_idx) {
                        for k in (0..n_undo).rev() {
                            gen.regs[undo_r[k] as usize] = undo_v[k];
                        }
                    }
                    return None;
                };
                if let HeapObj::Generator(gen) = self.heap.get_mut(js_idx) {
                    gen.state = GenState::Completed;
                    gen.regs.clear();
                    gen.handlers.clear();
                }
                let r = if ret.is_undefined() { Value::NULL } else { ret };
                if r.is_heap() {
                    self.heap.write_barrier_val(g, r);
                }
                if let HeapObj::Object(m) = self.heap.get_mut(g) {
                    m.set_val_at(F_DONE.0, Value::TRUE);
                    m.set_val_at(F_RETURNED.0, r);
                }
                if self.instr_rec.is_some() {
                    self.charge_steps(steps);
                }
                Some(rt.stop)
            }
            GenStepDone::Bail => None,
        }
    }

    /// [`Vm::py_gen_next`] for the state that matters for speed: a record of
    /// the shape already seen with every field at its usual slot (so read
    /// together), not done, not running, holding no exceptions, around an
    /// engine generator. `None` for anything else, with nothing done.
    #[inline]
    fn py_gen_next_plain(&mut self, this: Value) -> Option<Result<Value, Thrown>> {
        if let Some(v) = self.py_gen_next_fast(this) {
            return Some(Ok(v));
        }
        if !this.is_heap() {
            return None;
        }
        let g = this.heap_index();
        let shape = self.py_rt.as_deref()?.gen_shape.get();
        let HeapObj::Object(m) = self.heap.get(g) else {
            return None;
        };
        if m.is_ctor || shape == crate::shape::DICT || m.shape() != shape {
            return None;
        }
        let (js, done, started, running, excs, grt) =
            (m.val_at(F_JS.0), m.val_at(F_DONE.0), m.val_at(F_STARTED.0), m.val_at(F_RUNNING.0), m.val_at(F_EXCS.0), m.val_at(F_GRT.0));
        if done != Value::FALSE || running != Value::FALSE || excs != Value::NULL || !js.is_heap() {
            return None;
        }
        if !matches!(self.heap.get(js.heap_index()), HeapObj::Generator(_)) {
            return None;
        }
        let rt = self.py_gen_runtime_cached(grt)?;
        let depth = self.py_exc_depth(rt.exc_stack)?;
        // `running = true`, `started = true` (no barrier: not heap values).
        if let HeapObj::Object(m) = self.heap.get_mut(g) {
            m.set_val_at(F_RUNNING.0, Value::TRUE);
            if started != Value::TRUE {
                m.set_val_at(F_STARTED.0, Value::TRUE);
            }
        }
        Some(self.py_gen_step(this, js.heap_index(), &rt, depth, (F_DONE.0, F_RUNNING.0, F_RETURNED.0, F_EXCS.0)))
    }

    /// `__zipp_py_gen`, called as `g.next()`: one step of the Python
    /// generator record `this` (see the module comment).
    pub(crate) fn py_gen_next(&mut self, this: Value) -> Result<Value, Thrown> {
        if let Some(r) = self.py_gen_next_plain(this) {
            return r;
        }
        if let Some(v) = self.py_list_iter_step(this, true) {
            return Ok(v);
        }
        if !this.is_heap() || !matches!(self.heap.get(this.heap_index()), HeapObj::Object(m) if !m.is_ctor) {
            return Err(Thrown::from_static("TypeError: generator step on a non-generator"));
        }
        let g = this.heap_index();
        // A record of the shape already seen with every field at its usual
        // slot has them all there (a shape fixes keys, order and kinds).
        let known = match self.heap.get(g) {
            HeapObj::Object(m) => m.shape() != crate::shape::DICT && self.py_rt.as_deref().is_some_and(|p| p.gen_shape.get() == m.shape()),
            _ => false,
        };
        let rt = if known {
            self.py_gen_runtime(self.py_gen_read(g, F_GRT.0))
        } else {
            self.py_gen_slot(g, F_GRT)
                .and_then(|s| self.py_gen_runtime(self.py_gen_read(g, s)))
        };
        let Some(rt) = rt else {
            return Err(Thrown::from_static("TypeError: generator step on a non-generator"));
        };
        let slots = if known {
            Some((F_JS.0, F_DONE.0, F_STARTED.0, F_RUNNING.0, F_RETURNED.0, F_EXCS.0))
        } else {
            (|| {
                Some((
                    self.py_gen_slot(g, F_JS)?,
                    self.py_gen_slot(g, F_DONE)?,
                    self.py_gen_slot(g, F_STARTED)?,
                    self.py_gen_slot(g, F_RUNNING)?,
                    self.py_gen_slot(g, F_RETURNED)?,
                    self.py_gen_slot(g, F_EXCS)?,
                ))
            })()
        };
        if !known && slots == Some((F_JS.0, F_DONE.0, F_STARTED.0, F_RUNNING.0, F_RETURNED.0, F_EXCS.0)) && self.py_gen_slot(g, F_GRT) == Some(F_GRT.0) {
            if let HeapObj::Object(m) = self.heap.get(g) {
                if m.shape_guardable() {
                    let shape = m.shape();
                    if let Some(p) = self.py_rt.as_deref() {
                        p.gen_shape.set(shape);
                    }
                }
            }
        }
        let Some((s_js, s_done, s_started, s_running, s_returned, s_excs)) = slots else {
            return self.call_value(rt.gen_next, this, &[]);
        };
        let done = self.py_gen_read(g, s_done);
        if done == Value::TRUE {
            self.py_gen_write(g, s_returned, Value::NULL);
            return Ok(rt.stop);
        }
        let js = self.py_gen_read(g, s_js);
        let plain = done == Value::FALSE
            && self.py_gen_read(g, s_running) == Value::FALSE
            && self.py_gen_read(g, s_excs) == Value::NULL
            && js.is_heap()
            && matches!(self.heap.get(js.heap_index()), HeapObj::Generator(_));
        let depth = self.py_exc_depth(rt.exc_stack);
        let (true, Some(depth)) = (plain, depth) else {
            return self.call_value(rt.gen_next, this, &[]);
        };
        self.py_gen_write(g, s_running, Value::TRUE);
        if self.py_gen_read(g, s_started) != Value::TRUE {
            self.py_gen_write(g, s_started, Value::TRUE);
        }
        self.py_gen_step(this, js.heap_index(), &rt, depth, (s_done, s_running, s_returned, s_excs))
    }

    /// The rest of a plain step of the record `this` once `running` is set:
    /// the engine generator `js` resumed, then `genNext`'s bookkeeping
    /// (see the module comment). `slots`: the record's `done`, `running`,
    /// `returned` and `excs`.
    fn py_gen_step(&mut self, this: Value, js: u32, rt: &GenRuntime, depth: usize, slots: (usize, usize, usize, usize)) -> Result<Value, Thrown> {
        let g = this.heap_index();
        let (s_done, s_running, s_returned, s_excs) = slots;
        // The engine's `next` step, its iterator result not built.
        let step = self.generator_next_step(js);
        let res = match step {
            Ok(Some(super::async_runtime::GenStep::Yield(v))) => Ok((v, false)),
            Ok(Some(super::async_runtime::GenStep::Done(v))) => Ok((v, true)),
            Ok(Some(super::async_runtime::GenStep::Raw(res))) => Err(res),
            Ok(None) => Err(Value::UNDEFINED),
            Err(t) => {
                // `catch (e) { excStack.length = depth; throw genEscape(g, e); }`
                let e = match self.pending_throw.take() {
                    Some(v) => v,
                    None => {
                        let v = self.alloc_error_from_message(&t.0);
                        self.realm_adopt_error(v);
                        v
                    }
                };
                self.py_exc_truncate(rt.exc_stack, depth)?;
                let converted = self.call_value(rt.escape, Value::UNDEFINED, &[this, e])?;
                self.pending_throw = Some(converted);
                return Err(Thrown(self.throw_message(converted)));
            }
        };
        let (value, finished) = match res {
            Ok(step) => step,
            Err(res) => match self.iter_result_unwrap(res) {
                Some((v, d)) => (v, self.truthy(d)),
                None => {
                    let d = self.get_prop(res, "done")?;
                    let v = self.get_prop(res, "value")?;
                    (v, self.truthy(d))
                }
            },
        };
        // `if (!r.done && excStack.length > depth) g.excs = excStack.splice(depth);
        //  else if (excStack.length !== depth) excStack.length = depth;`
        let now = self.py_exc_depth(rt.exc_stack).unwrap_or(depth);
        if !finished && now > depth {
            let kept = match self.heap.get(rt.exc_stack) {
                HeapObj::Array(items) => items[depth..].to_vec(),
                _ => Vec::new(),
            };
            let arr = self.heap.alloc(HeapObj::Array(kept));
            self.py_gen_write(g, s_excs, Value::heap(arr));
            self.py_exc_truncate(rt.exc_stack, depth)?;
        } else if now != depth {
            self.py_exc_truncate(rt.exc_stack, depth)?;
        }
        self.py_gen_write(g, s_running, Value::FALSE);
        if finished {
            // genFinish: `done = true; returned = r.value ?? null` (undefined only).
            self.py_gen_write(g, s_done, Value::TRUE);
            let r = if value.is_undefined() { Value::NULL } else { value };
            self.py_gen_write(g, s_returned, r);
            return Ok(rt.stop);
        }
        Ok(value)
    }

    /// `excStack.length = depth`, as the runtime writes it.
    fn py_exc_truncate(&mut self, stack: u32, depth: usize) -> Result<(), Thrown> {
        self.set_prop(Value::heap(stack), "length", Value::num(depth as f64), true)?;
        Ok(())
    }
}
