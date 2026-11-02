//! Tier O's IR and its construction from bytecode.
//!
//! Steps, over the function's reachable blocks in reverse postorder:
//! bytecode liveness per position (frame states name only live registers,
//! phis exist only for live ones), a forward dataflow fixing each live
//! register's representation at every block entry (so a loop's phis know
//! theirs before the loop body is built), then SSA construction: each
//! bytecode op becomes IR values, its operands converted to the
//! representation it computes in, every conversion that can fail a guard
//! with the frame state of the op's own position. A multi-predecessor
//! block gets a phi per live register; each predecessor converts its value
//! to the phi's representation (only widenings: i32 to f64, anything to a
//! boxed value, which cannot fail) before its terminator.
//!
//! A call whose site has one plain user-function callee (`Site`) is
//! inlined when the callee is straight-line code Tier O admits, behind a
//! guard on the callee's function id (and, for a method, the receiver's
//! shape): every guard of the inlined body resumes the interpreter at the
//! call itself, which runs it again whole (the body has no effect to
//! repeat). Any other call is a call-out through the interpreter cache's
//! native helper, with the frame state written into the window first: the
//! callee's arguments, the collector's roots, and where the interpreter
//! goes on if the call throws or declines.

use rustc_hash::FxHashMap;

use crate::bytecode::{BitwiseOp, FuncProto, Instr};
use crate::value::Value;

use super::PropShapePlan;

/// An SSA value: an index into `Func::insts`.
pub(crate) type V = u32;
/// A block: an index into `Func::blocks` (reverse postorder; 0 the entry).
pub(crate) type B = u32;

/// A value's representation (`Void`: a guard with no value).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Rep {
    I32,
    F64,
    Bool,
    Tagged,
    Void,
}

impl Rep {
    /// The representation that holds both: i32 widens to f64, anything else
    /// mixed is boxed.
    fn join(a: Rep, b: Rep) -> Rep {
        match (a, b) {
            (x, y) if x == y => x,
            (Rep::I32, Rep::F64) | (Rep::F64, Rep::I32) => Rep::F64,
            _ => Rep::Tagged,
        }
    }
}

/// A Number comparison.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Cmp {
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
}

/// Which bytecode call a call-out makes, so which helper.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CallKind {
    /// `f(args)`.
    Plain,
    /// `o.m(args)`.
    Method,
    /// `f(args)` with the `this` of a register (`o.m(args)` whose callee was
    /// read before its arguments).
    WithThis,
}

/// A call site's one plain user-function callee, from the cross-call plan.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Site {
    Call { fid: u32 },
    /// An own data method: the receiver's shape and the slot holding it.
    Method { shape: u32, slot: u32, fid: u32 },
    /// A method of the receiver's class (`ClassReadLane`): the receiver's
    /// shape (no own property of the key), its class and that class's
    /// version, and the class-prototype epoch fix which method it is.
    ClassMethod {
        recv_shape: u32,
        class: u32,
        ver: u32,
        epoch: u32,
        fid: u32,
    },
}

impl Site {
    pub(crate) fn fid(self) -> u32 {
        match self {
            Site::Call { fid } | Site::Method { fid, .. } | Site::ClassMethod { fid, .. } => fid,
        }
    }

    fn method(self) -> bool {
        !matches!(self, Site::Call { .. })
    }
}

/// What a compile knows besides the function: its property and call sites,
/// and the bodies of the callees it may inline (each with its own
/// property plan).
pub(crate) struct Inputs<'a> {
    pub(crate) prop_shape: &'a PropShapePlan,
    pub(crate) sites: &'a FxHashMap<usize, Site>,
    pub(crate) callees: &'a FxHashMap<u32, (&'a FuncProto, PropShapePlan)>,
}

#[derive(Clone, Debug)]
pub(crate) enum Op {
    ConstI32(i32),
    ConstF64(f64),
    ConstBool(bool),
    ConstTagged(u64),
    /// The window's register at entry: `this` or a parameter.
    Param(u16),
    /// Inputs in the block's `preds` order.
    Phi(Vec<V>),
    I32ToF64(V),
    /// A Number unboxed (an Int converted); anything else fails the guard.
    TaggedToF64(V),
    /// ToInt32 of a double; NaN, the infinities and |x| >= 2^63 fail.
    F64ToI32(V),
    /// Any representation boxed (a double by `Value::num`'s rule).
    Box(V),
    /// A primitive's truthiness; a heap value fails.
    TaggedTruthy(V),
    F64Truthy(V),
    I32Truthy(V),
    AddF(V, V),
    SubF(V, V),
    MulF(V, V),
    DivF(V, V),
    NegF(V),
    /// `&`, `|`, `^`, `<<`, `>>` on i32s.
    Bit(BitwiseOp, V, V),
    /// `a >>> b`, a Number.
    UshrF(V, V),
    CmpF(Cmp, V, V),
    /// `===` (true) or `!==` of two booleans.
    CmpB(bool, V, V),
    NotB(V),
    /// `===` (true) or `!==` of two boxed values; two different heap values
    /// (strings may be equal) fail.
    StrictEqT(bool, V, V),
    /// An own data property read behind the receiver's shape; anything else
    /// fails.
    GetPropShape { obj: V, shape: u32, slot: u32 },
    /// A global's slot; a binding in its TDZ fails.
    LoadGlobal(u32),
    /// `v` is a function of id `fid`; anything else fails.
    GuardFid { v: V, fid: u32 },
    /// `obj` is an instance of `class` of shape `recv_shape`, the class at
    /// version `ver` and the class prototypes at `epoch`; else fails.
    GuardClass {
        obj: V,
        recv_shape: u32,
        class: u32,
        ver: u32,
        epoch: u32,
    },
    /// The call at `ip` through its native helper (`CallKind`), the frame
    /// state written first (`Inst::fs`); its result boxed.
    CallOut { kind: CallKind, ip: u32, packed: u64, argc: u16 },
}

impl Op {
    /// The values this op reads (phi inputs are not: they are read at the
    /// end of each predecessor).
    pub(crate) fn operands(&self) -> Vec<V> {
        match *self {
            Op::ConstI32(_)
            | Op::ConstF64(_)
            | Op::ConstBool(_)
            | Op::ConstTagged(_)
            | Op::Param(_)
            | Op::Phi(_)
            | Op::LoadGlobal(_)
            | Op::CallOut { .. } => vec![],
            Op::I32ToF64(a)
            | Op::TaggedToF64(a)
            | Op::F64ToI32(a)
            | Op::Box(a)
            | Op::TaggedTruthy(a)
            | Op::F64Truthy(a)
            | Op::I32Truthy(a)
            | Op::NegF(a)
            | Op::NotB(a)
            | Op::GetPropShape { obj: a, .. }
            | Op::GuardFid { v: a, .. }
            | Op::GuardClass { obj: a, .. } => vec![a],
            Op::AddF(a, b)
            | Op::SubF(a, b)
            | Op::MulF(a, b)
            | Op::DivF(a, b)
            | Op::Bit(_, a, b)
            | Op::UshrF(a, b)
            | Op::CmpF(_, a, b)
            | Op::CmpB(_, a, b)
            | Op::StrictEqT(_, a, b) => vec![a, b],
        }
    }
}

pub(crate) struct Inst {
    pub(crate) op: Op,
    pub(crate) rep: Rep,
    /// The frame state a failed guard of this op deopts with (a call-out's:
    /// the one written before the call).
    pub(crate) fs: Option<u32>,
    pub(crate) block: B,
}

pub(crate) enum Term {
    Jump(B),
    /// On a Bool: `t` when true.
    Branch { cond: V, t: B, f: B },
    /// A boxed value.
    Return(V),
}

impl Term {
    pub(crate) fn succs(&self) -> Vec<B> {
        match *self {
            Term::Jump(s) => vec![s],
            Term::Branch { t, f, .. } => vec![t, f],
            Term::Return(_) => vec![],
        }
    }
}

pub(crate) struct Block {
    /// The phis, then the block's other values, in order.
    pub(crate) insts: Vec<V>,
    pub(crate) term: Term,
    pub(crate) preds: Vec<B>,
}

/// What a failed guard writes back: the position to resume at and, for
/// each VM register live there, the value it holds.
pub(crate) struct FrameState {
    pub(crate) ip: u32,
    pub(crate) regs: Vec<(u16, V)>,
}

pub(crate) struct Func {
    pub(crate) func_id: u32,
    pub(crate) insts: Vec<Inst>,
    pub(crate) blocks: Vec<Block>,
    pub(crate) states: Vec<FrameState>,
    /// Calls inlined, for the log.
    pub(crate) inlined: u32,
}

/// Bodies larger than this are not worth the compile (and keep the bitsets
/// small).
const MAX_OPS: usize = 4096;
const MAX_REGS: usize = 1024;
/// The largest callee inlined.
const MAX_INLINE_OPS: usize = 64;

/// The registers `i` at `ip` of `p` reads and the one it writes, for the ops
/// Tier O admits; `None` for any other op. `sites` admits the calls (none
/// inside an inlined callee).
fn uses_def(
    i: &Instr,
    p: &FuncProto,
    prop_shape: &PropShapePlan,
    sites: Option<&FxHashMap<usize, Site>>,
    ip: usize,
) -> Option<(Vec<u16>, Option<u16>)> {
    let args = |first: u16, base: u16, argc: u16| -> Vec<u16> {
        std::iter::once(first).chain(base..base + argc).collect()
    };
    Some(match *i {
        Instr::LoadInt { dst, .. } | Instr::LoadBool { dst, .. } | Instr::LoadNull { dst } | Instr::LoadUndefined { dst } => {
            (vec![], Some(dst))
        }
        Instr::LoadConst { dst, idx } => {
            if !p.constants.get(idx as usize).is_some_and(|c| c.is_number()) {
                return None;
            }
            (vec![], Some(dst))
        }
        Instr::LoadGlobal { dst, .. } if sites.is_some() => (vec![], Some(dst)),
        Instr::Move { dst, src } => (vec![src], Some(dst)),
        Instr::Add { dst, a, b }
        | Instr::Sub { dst, a, b }
        | Instr::Mul { dst, a, b }
        | Instr::Div { dst, a, b }
        | Instr::Lt { dst, a, b }
        | Instr::Le { dst, a, b }
        | Instr::Gt { dst, a, b }
        | Instr::Ge { dst, a, b }
        | Instr::Eq { dst, a, b }
        | Instr::Ne { dst, a, b }
        | Instr::Bitwise { dst, a, b, .. } => (vec![a, b], Some(dst)),
        Instr::AddInt { dst, a, .. } | Instr::Neg { dst, a } | Instr::Not { dst, a } => (vec![a], Some(dst)),
        Instr::GetProp { dst, obj, .. } => {
            if !prop_shape.contains_key(&ip) {
                return None;
            }
            (vec![obj], Some(dst))
        }
        Instr::Call {
            dst,
            callee,
            arg_base,
            argc,
        } if sites.is_some_and(|s| matches!(s.get(&ip), Some(Site::Call { .. }))) => {
            (args(callee, arg_base, argc), Some(dst))
        }
        Instr::CallWithThis {
            dst,
            callee,
            this_v,
            arg_base,
            argc,
            ..
        } if sites.is_some_and(|s| matches!(s.get(&ip), Some(Site::Call { .. }))) => {
            let mut u = args(callee, arg_base, argc);
            u.push(this_v);
            (u, Some(dst))
        }
        Instr::CallMethod {
            dst,
            obj,
            arg_base,
            argc,
            ..
        } if sites.is_some_and(|s| matches!(s.get(&ip), Some(Site::Method { .. } | Site::ClassMethod { .. }))) => {
            (args(obj, arg_base, argc), Some(dst))
        }
        Instr::Jump { .. } | Instr::ReturnUndefined => (vec![], None),
        Instr::JumpIfFalse { cond, .. } | Instr::JumpIfTrue { cond, .. } => (vec![cond], None),
        Instr::JumpIfNotLt { a, b, .. } | Instr::JumpIfNotLe { a, b, .. } => (vec![a, b], None),
        Instr::Return { src } => (vec![src], None),
        _ => return None,
    })
}

/// Where control goes after `i` at `ip`: its fall-through (if any) and its
/// jump target (if any).
fn flow(i: &Instr, ip: usize) -> (Option<usize>, Option<usize>) {
    match *i {
        Instr::Jump { target } => (None, Some(target as usize)),
        Instr::JumpIfFalse { target, .. }
        | Instr::JumpIfTrue { target, .. }
        | Instr::JumpIfNotLt { target, .. }
        | Instr::JumpIfNotLe { target, .. } => (Some(ip + 1), Some(target as usize)),
        Instr::Return { .. } | Instr::ReturnUndefined => (None, None),
        _ => (Some(ip + 1), None),
    }
}

/// The representation `i` makes its destination, given the registers'
/// (`reps`); a call's is `call` (an inlined callee's result, else boxed).
fn result_rep(i: &Instr, reps: &[Rep], p: &FuncProto, call: Rep) -> Option<Rep> {
    Some(match *i {
        Instr::LoadInt { .. } => Rep::I32,
        Instr::LoadConst { idx, .. } => {
            if p.constants[idx as usize].is_int() {
                Rep::I32
            } else {
                Rep::F64
            }
        }
        Instr::LoadBool { .. }
        | Instr::Lt { .. }
        | Instr::Le { .. }
        | Instr::Gt { .. }
        | Instr::Ge { .. }
        | Instr::Eq { .. }
        | Instr::Ne { .. }
        | Instr::Not { .. } => Rep::Bool,
        Instr::LoadNull { .. } | Instr::LoadUndefined { .. } | Instr::GetProp { .. } | Instr::LoadGlobal { .. } => {
            Rep::Tagged
        }
        Instr::Move { src, .. } => reps[src as usize],
        Instr::Add { .. } | Instr::Sub { .. } | Instr::Mul { .. } | Instr::Div { .. } | Instr::AddInt { .. } | Instr::Neg { .. } => {
            Rep::F64
        }
        Instr::Bitwise { op, .. } => {
            if matches!(op, BitwiseOp::Ushr) {
                Rep::F64
            } else {
                Rep::I32
            }
        }
        Instr::Call { .. } | Instr::CallMethod { .. } | Instr::CallWithThis { .. } => call,
        _ => return None,
    })
}

/// A callee Tier O inlines at a call site: straight-line code it admits
/// (no jump, no call, no global, no captured variable) up to its first
/// return, small, with a `this` the inlined body can be given: the
/// receiver of a method call, `undefined` for a strict function's plain
/// call; an arrow or a sloppy plain call that reads its `this` is not.
fn inlinable(c: &FuncProto, plan: &PropShapePlan, method: bool) -> bool {
    if c.is_generator
        || c.is_async
        || c.rest_reg.is_some()
        || c.arguments_reg.is_some()
        || !c.eval_sites.is_empty()
        || c.code.len() > MAX_INLINE_OPS
        || c.reg_count as usize > MAX_INLINE_OPS
        || (c.param_count as usize) >= (c.reg_count as usize).max(1)
    {
        return false;
    }
    let mut reads_this = false;
    for (ip, i) in c.code.iter().enumerate() {
        if matches!(i, Instr::Return { .. } | Instr::ReturnUndefined) {
            if let Instr::Return { src: 0 } = i {
                reads_this = true;
            }
            break;
        }
        let Some((uses, def)) = uses_def(i, c, plan, None, ip) else {
            return false;
        };
        if flow(i, ip).1.is_some() || uses.iter().chain(def.iter()).any(|&r| r >= c.reg_count) {
            return false;
        }
        reads_this |= uses.contains(&0);
    }
    if !c.code.iter().any(|i| matches!(i, Instr::Return { .. } | Instr::ReturnUndefined)) {
        return false;
    }
    !reads_this || (!c.lexical_this && (method || c.is_strict))
}

/// The representation an inlined straight-line callee returns, its
/// registers entering with `reps`.
fn inline_result_rep(c: &FuncProto, mut reps: Vec<Rep>) -> Rep {
    for i in &c.code {
        match *i {
            Instr::Return { src } => return reps[src as usize],
            Instr::ReturnUndefined => return Rep::Tagged,
            _ => {}
        }
        if let (Some(r), Some(d)) = (result_rep(i, &reps, c, Rep::Tagged), dst_of(i)) {
            reps[d as usize] = r;
        }
    }
    Rep::Tagged
}

fn dst_of(i: &Instr) -> Option<u16> {
    match *i {
        Instr::LoadInt { dst, .. }
        | Instr::LoadBool { dst, .. }
        | Instr::LoadNull { dst }
        | Instr::LoadUndefined { dst }
        | Instr::LoadConst { dst, .. }
        | Instr::LoadGlobal { dst, .. }
        | Instr::Move { dst, .. }
        | Instr::Add { dst, .. }
        | Instr::Sub { dst, .. }
        | Instr::Mul { dst, .. }
        | Instr::Div { dst, .. }
        | Instr::Lt { dst, .. }
        | Instr::Le { dst, .. }
        | Instr::Gt { dst, .. }
        | Instr::Ge { dst, .. }
        | Instr::Eq { dst, .. }
        | Instr::Ne { dst, .. }
        | Instr::Bitwise { dst, .. }
        | Instr::AddInt { dst, .. }
        | Instr::Neg { dst, .. }
        | Instr::Not { dst, .. }
        | Instr::GetProp { dst, .. }
        | Instr::Call { dst, .. }
        | Instr::CallMethod { dst, .. }
        | Instr::CallWithThis { dst, .. } => Some(dst),
        _ => None,
    }
}

/// A fixed-width register set.
#[derive(Clone, PartialEq, Eq)]
struct Bits(Vec<u64>);

impl Bits {
    fn new(n: usize) -> Bits {
        Bits(vec![0; n.div_ceil(64).max(1)])
    }
    fn get(&self, r: usize) -> bool {
        self.0[r / 64] >> (r % 64) & 1 != 0
    }
    fn set(&mut self, r: usize) {
        self.0[r / 64] |= 1 << (r % 64);
    }
    fn clear(&mut self, r: usize) {
        self.0[r / 64] &= !(1 << (r % 64));
    }
    fn union(&mut self, o: &Bits) -> bool {
        let mut changed = false;
        for (a, b) in self.0.iter_mut().zip(&o.0) {
            let n = *a | *b;
            changed |= n != *a;
            *a = n;
        }
        changed
    }
}

/// Where one op sends control (`translate`).
enum Flow {
    Next,
    Jump(usize),
    /// On a Bool, to positions.
    Branch {
        cond: V,
        t: usize,
        f: usize,
    },
    /// The value returned, in its own representation.
    Ret(V),
}

/// The function a translation reads its constants and plans from.
struct Fn<'a> {
    p: &'a FuncProto,
    plan: &'a PropShapePlan,
}

/// The IR of `proto`, or `None` when Tier O does not admit it.
pub(crate) fn build(p: &FuncProto, func_id: u32, inputs: &Inputs) -> Option<Func> {
    let code = &p.code;
    let n = code.len();
    let nregs = p.reg_count as usize;
    if n == 0
        || n > MAX_OPS
        || nregs > MAX_REGS
        || p.is_generator
        || p.is_async
        || p.rest_reg.is_some()
        || p.arguments_reg.is_some()
        || !p.eval_sites.is_empty()
        || (p.param_count as usize) >= nregs.max(1)
    {
        return None;
    }
    let mut ud = Vec::with_capacity(n);
    for (ip, i) in code.iter().enumerate() {
        let Some((uses, def)) = uses_def(i, p, inputs.prop_shape, Some(inputs.sites), ip) else {
            if crate::jitlog_enabled() {
                eprintln!("[jit] Tier O fn{func_id} declines at ip {ip}: {i:?}");
            }
            return None;
        };
        if uses.iter().chain(def.iter()).any(|&r| r as usize >= nregs) {
            return None;
        }
        ud.push((uses, def));
    }
    // Every path ends at a return.
    if !matches!(code[n - 1], Instr::Return { .. } | Instr::ReturnUndefined) {
        return None;
    }
    for (ip, i) in code.iter().enumerate() {
        let (_, t) = flow(i, ip);
        if t.is_some_and(|t| t >= n) {
            return None;
        }
    }
    // The call sites inlined: the callee, and whether it is a method.
    let inline_at: FxHashMap<usize, (&FuncProto, &PropShapePlan, Site)> = inputs
        .sites
        .iter()
        .filter_map(|(&ip, &site)| {
            let (c, plan) = inputs.callees.get(&site.fid())?;
            let method = site.method();
            let ok = match code.get(ip) {
                Some(Instr::Call { .. } | Instr::CallWithThis { .. }) => !method,
                Some(Instr::CallMethod { .. }) => method,
                _ => false,
            };
            (ok && inlinable(c, plan, method)).then_some((ip, (*c, plan, site)))
        })
        .collect();

    // ── blocks ── leaders: the entry, every jump target, and whatever
    // follows a jump or a return.
    let mut leader = vec![false; n];
    leader[0] = true;
    for (ip, i) in code.iter().enumerate() {
        let (ft, t) = flow(i, ip);
        if let Some(t) = t {
            leader[t] = true;
            if ip + 1 < n {
                leader[ip + 1] = true;
            }
        }
        if ft.is_none() && ip + 1 < n {
            leader[ip + 1] = true;
        }
    }
    let starts: Vec<usize> = (0..n).filter(|&ip| leader[ip]).collect();
    let block_of_ip = |ip: usize| starts.partition_point(|&s| s <= ip) - 1;
    let nb = starts.len();
    let end_of = |b: usize| if b + 1 < nb { starts[b + 1] } else { n };
    let succs_of = |b: usize| -> Vec<usize> {
        let last = end_of(b) - 1;
        let (ft, t) = flow(&code[last], last);
        let mut s = Vec::new();
        if let Some(f) = ft {
            if f < n {
                s.push(block_of_ip(f));
            }
        }
        if let Some(t) = t {
            s.push(block_of_ip(t));
        }
        s
    };
    let bsuccs: Vec<Vec<usize>> = (0..nb).map(succs_of).collect();
    // Reverse postorder over the reachable blocks.
    let mut order = Vec::new();
    let mut seen = vec![false; nb];
    let mut stack: Vec<(usize, usize)> = vec![(0, 0)];
    seen[0] = true;
    while let Some(&mut (b, ref mut k)) = stack.last_mut() {
        if *k < bsuccs[b].len() {
            let s = bsuccs[b][*k];
            *k += 1;
            if !seen[s] {
                seen[s] = true;
                stack.push((s, 0));
            }
        } else {
            order.push(b);
            stack.pop();
        }
    }
    order.reverse();
    let mut rpo_index = vec![usize::MAX; nb];
    for (k, &b) in order.iter().enumerate() {
        rpo_index[b] = k;
    }
    let mut bpreds: Vec<Vec<usize>> = vec![Vec::new(); nb];
    for &b in &order {
        for &s in &bsuccs[b] {
            bpreds[s].push(b);
        }
    }

    // ── liveness ── per block, then per position (live before each op).
    let mut live_in: Vec<Bits> = vec![Bits::new(nregs); nb];
    let mut changed = true;
    while changed {
        changed = false;
        for &b in order.iter().rev() {
            let mut live = Bits::new(nregs);
            for &s in &bsuccs[b] {
                live.union(&live_in[s]);
            }
            for ip in (starts[b]..end_of(b)).rev() {
                let (uses, def) = &ud[ip];
                if let Some(d) = def {
                    live.clear(*d as usize);
                }
                for &u in uses {
                    live.set(u as usize);
                }
            }
            if live != live_in[b] {
                live_in[b] = live;
                changed = true;
            }
        }
    }
    let mut live_before: Vec<Option<Bits>> = vec![None; n];
    for &b in &order {
        let mut live = Bits::new(nregs);
        for &s in &bsuccs[b] {
            live.union(&live_in[s]);
        }
        for ip in (starts[b]..end_of(b)).rev() {
            let (uses, def) = &ud[ip];
            if let Some(d) = def {
                live.clear(*d as usize);
            }
            for &u in uses {
                live.set(u as usize);
            }
            live_before[ip] = Some(live.clone());
        }
    }

    // ── parameter speculation ── a parameter the body only computes with
    // (read by a Number op, never as an object or call operand, never moved,
    // written only with Numbers) is unboxed once at entry, behind a guard
    // that resumes the call at its first op. Unboxed at each use instead, a
    // loop paid the guard every iteration, and a loop variable seeded with
    // it merged a box with Numbers at its phi and was reboxed every
    // iteration.
    let mut spec = vec![false; nregs];
    for r in 1..=(p.param_count as usize).min(nregs.saturating_sub(1)) {
        let r16 = r as u16;
        let mut numeric_read = false;
        let mut ok = true;
        for (ip, i) in code.iter().enumerate() {
            match *i {
                Instr::Add { a, b, .. }
                | Instr::Sub { a, b, .. }
                | Instr::Mul { a, b, .. }
                | Instr::Div { a, b, .. }
                | Instr::Lt { a, b, .. }
                | Instr::Le { a, b, .. }
                | Instr::Gt { a, b, .. }
                | Instr::Ge { a, b, .. }
                | Instr::Bitwise { a, b, .. }
                | Instr::JumpIfNotLt { a, b, .. }
                | Instr::JumpIfNotLe { a, b, .. }
                    if a == r16 || b == r16 =>
                {
                    numeric_read = true
                }
                Instr::AddInt { a, .. } | Instr::Neg { a, .. } if a == r16 => numeric_read = true,
                Instr::GetProp { obj, .. } if obj == r16 => ok = false,
                Instr::Move { src, .. } if src == r16 => ok = false,
                Instr::Call { .. } | Instr::CallMethod { .. } | Instr::CallWithThis { .. } if ud[ip].0.contains(&r16) => ok = false,
                _ => {}
            }
            if let Some(d) = ud[ip].1 {
                let numeric_write = matches!(
                    i,
                    Instr::Add { .. }
                        | Instr::Sub { .. }
                        | Instr::Mul { .. }
                        | Instr::Div { .. }
                        | Instr::AddInt { .. }
                        | Instr::Neg { .. }
                        | Instr::Bitwise { .. }
                        | Instr::LoadInt { .. }
                        | Instr::LoadConst { .. }
                );
                if d == r16 && !numeric_write {
                    ok = false;
                }
            }
        }
        spec[r] = ok && numeric_read && live_before[0].as_ref().is_some_and(|l| l.get(r));
    }

    // ── representations ── each live register's at each block entry. An
    // inlined call's is its callee's result's, given its arguments'.
    let call_rep = |ip: usize, reps: &[Rep]| -> Rep {
        let Some(&(c, _, site)) = inline_at.get(&ip) else {
            return Rep::Tagged;
        };
        // The inlined body's `this` is boxed (the receiver, or undefined).
        let (arg_base, argc) = match &code[ip] {
            Instr::CallMethod { arg_base, argc, .. } if site.method() => (*arg_base, *argc),
            Instr::Call { arg_base, argc, .. } | Instr::CallWithThis { arg_base, argc, .. } if !site.method() => (*arg_base, *argc),
            _ => return Rep::Tagged,
        };
        let mut creps = vec![Rep::Tagged; c.reg_count as usize];
        for k in 0..(argc as usize).min(c.param_count as usize) {
            creps[1 + k] = reps[arg_base as usize + k];
        }
        inline_result_rep(c, creps)
    };
    // `None`: not yet reached.
    let mut in_rep: Vec<Option<Vec<Rep>>> = vec![None; nb];
    in_rep[0] = Some((0..nregs).map(|r| if spec[r] { Rep::F64 } else { Rep::Tagged }).collect());
    let mut changed = true;
    while changed {
        changed = false;
        for &b in &order {
            let Some(mut reps) = in_rep[b].clone() else {
                continue;
            };
            for ip in starts[b]..end_of(b) {
                let call = call_rep(ip, &reps);
                if let (Some(r), (_, Some(d))) = (result_rep(&code[ip], &reps, p, call), &ud[ip]) {
                    reps[*d as usize] = r;
                }
            }
            for &s in &bsuccs[b] {
                match &mut in_rep[s] {
                    None => {
                        in_rep[s] = Some(reps.clone());
                        changed = true;
                    }
                    Some(cur) => {
                        for r in 0..nregs {
                            if live_in[s].get(r) {
                                let j = Rep::join(cur[r], reps[r]);
                                if j != cur[r] {
                                    cur[r] = j;
                                    changed = true;
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // ── SSA ──
    let mut b = Builder {
        insts: Vec::new(),
        states: Vec::new(),
        cur_block: 0,
        memo: Vec::new(),
        out: Vec::new(),
        consts: Vec::new(),
        inlined: 0,
    };
    let top = Fn {
        p,
        plan: inputs.prop_shape,
    };
    let mut blocks: Vec<Block> = Vec::with_capacity(order.len());
    let mut exit_defs: Vec<Vec<V>> = vec![Vec::new(); order.len()];
    // Phis to fill once every block is built: (block, phi, register).
    let mut phis: Vec<(usize, V, u16)> = Vec::new();
    let undefined_bits = Value::UNDEFINED.bits();
    for (k, &bb) in order.iter().enumerate() {
        b.cur_block = k as B;
        b.memo.clear();
        let preds: Vec<B> = bpreds[bb].iter().map(|&p| rpo_index[p] as B).collect();
        let mut defs: Vec<V> = vec![V::MAX; nregs];
        let mut insts_of_block: Vec<V> = Vec::new();
        if k == 0 {
            debug_assert!(preds.is_empty(), "the entry has no predecessor");
            let undef = b.konst(Op::ConstTagged(undefined_bits), Rep::Tagged);
            for (r, d) in defs.iter_mut().enumerate() {
                *d = if r <= p.param_count as usize {
                    let v = b.push(Op::Param(r as u16), Rep::Tagged, None);
                    insts_of_block.push(v);
                    v
                } else {
                    undef
                };
            }
            // The speculated parameters (`spec`), unboxed behind one frame
            // state: the call's first op, its parameters as passed.
            let entry_live = live_before[starts[bb]].as_ref().expect("the entry has liveness");
            let unbox: Vec<usize> = (0..nregs).filter(|&r| spec[r]).collect();
            if !unbox.is_empty() {
                let s = b.states.len() as u32;
                b.states.push(FrameState {
                    ip: starts[bb] as u32,
                    regs: (0..nregs)
                        .filter(|&r| entry_live.get(r))
                        .map(|r| (r as u16, defs[r]))
                        .collect(),
                });
                for r in unbox {
                    let c = b.push(Op::TaggedToF64(defs[r]), Rep::F64, Some(s));
                    insts_of_block.push(c);
                    defs[r] = c;
                }
            }
        } else if preds.len() == 1 && (preds[0] as usize) < k {
            defs = exit_defs[preds[0] as usize].clone();
        } else {
            let reps = in_rep[bb].as_ref().expect("a reachable block has entry representations");
            for r in 0..nregs {
                if live_in[bb].get(r) {
                    let phi = b.push(Op::Phi(Vec::new()), reps[r], None);
                    insts_of_block.push(phi);
                    defs[r] = phi;
                    phis.push((k, phi, r as u16));
                }
            }
        }
        b.out = std::mem::take(&mut insts_of_block);
        let mut term = None;
        for ip in starts[bb]..end_of(bb) {
            let live = live_before[ip].as_ref().expect("every reachable position has liveness");
            let fs_regs: Vec<u16> = (0..nregs).filter(|&r| live.get(r)).map(|r| r as u16).collect();
            let mut fs = FsCtx {
                ip: ip as u32,
                regs: &fs_regs,
                id: None,
            };
            let inline = inline_at.get(&ip).copied();
            match translate(&mut b, &top, ip, &code[ip], &mut defs, &mut fs, inline)? {
                Flow::Next => {}
                Flow::Jump(t) => term = Some(Term::Jump(rpo_index[block_of_ip(t)] as B)),
                Flow::Branch { cond, t, f } => {
                    term = Some(Term::Branch {
                        cond,
                        t: rpo_index[block_of_ip(t)] as B,
                        f: rpo_index[block_of_ip(f)] as B,
                    })
                }
                Flow::Ret(v) => {
                    let v = b.tagged(v);
                    term = Some(Term::Return(v));
                }
            }
        }
        let term = term.unwrap_or_else(|| {
            // Falls through into the next block.
            Term::Jump(rpo_index[block_of_ip(end_of(bb))] as B)
        });
        exit_defs[k] = defs;
        blocks.push(Block {
            insts: std::mem::take(&mut b.out),
            term,
            preds,
        });
    }
    // ── phis ── each predecessor's value, widened to the phi's
    // representation before the predecessor's terminator.
    for (k, phi, r) in phis {
        let mut inputs = Vec::with_capacity(blocks[k].preds.len());
        for pi in 0..blocks[k].preds.len() {
            let pred = blocks[k].preds[pi] as usize;
            let v = exit_defs[pred][r as usize];
            debug_assert!(v != V::MAX, "a live register is defined on every path");
            let want = b.insts[phi as usize].rep;
            let have = b.insts[v as usize].rep;
            let v = if have == want {
                v
            } else {
                let op = match (have, want) {
                    (Rep::I32, Rep::F64) => Op::I32ToF64(v),
                    (_, Rep::Tagged) => Op::Box(v),
                    _ => unreachable!("a phi only widens: {have:?} to {want:?}"),
                };
                let c = b.push(op, want, None);
                b.insts[c as usize].block = pred as B;
                blocks[pred].insts.push(c);
                c
            };
            inputs.push(v);
        }
        b.insts[phi as usize].op = Op::Phi(inputs);
    }
    // The constants open the entry block.
    let mut entry: Vec<V> = b.consts.iter().map(|&(_, _, v)| v).collect();
    entry.append(&mut blocks[0].insts);
    blocks[0].insts = entry;
    Some(Func {
        func_id,
        insts: b.insts,
        blocks,
        states: b.states,
        inlined: b.inlined,
    })
}

/// One op of `f` at `ip` into the current block. `inline`: the call site's
/// callee, inlined. `None` when the op is not one Tier O builds.
fn translate(
    b: &mut Builder,
    f: &Fn,
    ip: usize,
    ins: &Instr,
    defs: &mut [V],
    fs: &mut FsCtx,
    inline: Option<(&FuncProto, &PropShapePlan, Site)>,
) -> Option<Flow> {
    let undefined_bits = Value::UNDEFINED.bits();
    match *ins {
        Instr::LoadInt { dst, val } => defs[dst as usize] = b.konst(Op::ConstI32(val), Rep::I32),
        Instr::LoadConst { dst, idx } => {
            let c = f.p.constants[idx as usize];
            defs[dst as usize] = if c.is_int() {
                b.konst(Op::ConstI32(c.as_int()), Rep::I32)
            } else {
                b.konst(Op::ConstF64(c.as_f64()), Rep::F64)
            };
        }
        Instr::LoadBool { dst, val } => defs[dst as usize] = b.konst(Op::ConstBool(val), Rep::Bool),
        Instr::LoadNull { dst } => defs[dst as usize] = b.konst(Op::ConstTagged(Value::NULL.bits()), Rep::Tagged),
        Instr::LoadUndefined { dst } => defs[dst as usize] = b.konst(Op::ConstTagged(undefined_bits), Rep::Tagged),
        Instr::LoadGlobal { dst, idx } => {
            let s = fs.id(b, defs);
            defs[dst as usize] = b.push_out(Op::LoadGlobal(idx), Rep::Tagged, Some(s));
        }
        Instr::Move { dst, src } => defs[dst as usize] = defs[src as usize],
        Instr::Add { dst, a, b: c } | Instr::Sub { dst, a, b: c } | Instr::Mul { dst, a, b: c } | Instr::Div { dst, a, b: c } => {
            let x = b.f64(defs[a as usize], fs, defs);
            let y = b.f64(defs[c as usize], fs, defs);
            let op = match *ins {
                Instr::Add { .. } => Op::AddF(x, y),
                Instr::Sub { .. } => Op::SubF(x, y),
                Instr::Mul { .. } => Op::MulF(x, y),
                _ => Op::DivF(x, y),
            };
            defs[dst as usize] = b.emit(op, Rep::F64);
        }
        Instr::AddInt { dst, a, imm, .. } => {
            let x = b.f64(defs[a as usize], fs, defs);
            let y = b.konst(Op::ConstF64(imm as f64), Rep::F64);
            defs[dst as usize] = b.emit(Op::AddF(x, y), Rep::F64);
        }
        Instr::Neg { dst, a } => {
            let x = b.f64(defs[a as usize], fs, defs);
            defs[dst as usize] = b.emit(Op::NegF(x), Rep::F64);
        }
        Instr::Lt { dst, a, b: c } | Instr::Le { dst, a, b: c } | Instr::Gt { dst, a, b: c } | Instr::Ge { dst, a, b: c } => {
            let cmp = match *ins {
                Instr::Lt { .. } => Cmp::Lt,
                Instr::Le { .. } => Cmp::Le,
                Instr::Gt { .. } => Cmp::Gt,
                _ => Cmp::Ge,
            };
            let x = b.f64(defs[a as usize], fs, defs);
            let y = b.f64(defs[c as usize], fs, defs);
            defs[dst as usize] = b.emit(Op::CmpF(cmp, x, y), Rep::Bool);
        }
        Instr::Eq { dst, a, b: c } | Instr::Ne { dst, a, b: c } => {
            let eq = matches!(*ins, Instr::Eq { .. });
            let (va, vc) = (defs[a as usize], defs[c as usize]);
            let (ra, rc) = (b.rep(va), b.rep(vc));
            let num = |r: Rep| matches!(r, Rep::I32 | Rep::F64);
            defs[dst as usize] = if num(ra) && num(rc) {
                let x = b.f64(va, fs, defs);
                let y = b.f64(vc, fs, defs);
                b.emit(Op::CmpF(if eq { Cmp::Eq } else { Cmp::Ne }, x, y), Rep::Bool)
            } else if ra == Rep::Bool && rc == Rep::Bool {
                b.emit(Op::CmpB(eq, va, vc), Rep::Bool)
            } else {
                let x = b.tagged(va);
                let y = b.tagged(vc);
                let s = fs.id(b, defs);
                b.push_out(Op::StrictEqT(eq, x, y), Rep::Bool, Some(s))
            };
        }
        Instr::Not { dst, a } => {
            let t = b.truthy(defs[a as usize], fs, defs);
            defs[dst as usize] = b.emit(Op::NotB(t), Rep::Bool);
        }
        Instr::Bitwise { dst, a, b: c, op } => {
            let x = b.i32(defs[a as usize], fs, defs);
            let y = b.i32(defs[c as usize], fs, defs);
            defs[dst as usize] = if matches!(op, BitwiseOp::Ushr) {
                b.emit(Op::UshrF(x, y), Rep::F64)
            } else {
                b.emit(Op::Bit(op, x, y), Rep::I32)
            };
        }
        Instr::GetProp { dst, obj, .. } => {
            let (shape, slot) = *f.plan.get(&ip)?;
            let o = b.tagged(defs[obj as usize]);
            let s = fs.id(b, defs);
            defs[dst as usize] = b.push_out(Op::GetPropShape { obj: o, shape, slot }, Rep::Tagged, Some(s));
        }
        Instr::Call {
            dst,
            callee,
            arg_base,
            argc,
        } => {
            let s = fs.id(b, defs);
            defs[dst as usize] = match inline {
                Some((c, plan, Site::Call { fid })) => {
                    let cv = b.tagged(defs[callee as usize]);
                    b.push_out(Op::GuardFid { v: cv, fid }, Rep::Void, Some(s));
                    let this = b.konst(Op::ConstTagged(undefined_bits), Rep::Tagged);
                    let args: Vec<V> = (0..argc).map(|k| defs[(arg_base + k) as usize]).collect();
                    b.inline(c, plan, this, &args, s)?
                }
                _ => b.push_out(
                    Op::CallOut {
                        kind: CallKind::Plain,
                        ip: ip as u32,
                        packed: ((callee as u64) << 16) | arg_base as u64,
                        argc,
                    },
                    Rep::Tagged,
                    Some(s),
                ),
            };
        }
        Instr::CallMethod {
            dst,
            obj,
            name,
            arg_base,
            argc,
        } => {
            let s = fs.id(b, defs);
            defs[dst as usize] = match inline {
                Some((c, plan, Site::Method { shape, slot, fid })) => {
                    let o = b.tagged(defs[obj as usize]);
                    let m = b.push_out(Op::GetPropShape { obj: o, shape, slot }, Rep::Tagged, Some(s));
                    b.push_out(Op::GuardFid { v: m, fid }, Rep::Void, Some(s));
                    let args: Vec<V> = (0..argc).map(|k| defs[(arg_base + k) as usize]).collect();
                    b.inline(c, plan, o, &args, s)?
                }
                Some((
                    c,
                    plan,
                    Site::ClassMethod {
                        recv_shape,
                        class,
                        ver,
                        epoch,
                        ..
                    },
                )) => {
                    let o = b.tagged(defs[obj as usize]);
                    b.push_out(
                        Op::GuardClass {
                            obj: o,
                            recv_shape,
                            class,
                            ver,
                            epoch,
                        },
                        Rep::Void,
                        Some(s),
                    );
                    let args: Vec<V> = (0..argc).map(|k| defs[(arg_base + k) as usize]).collect();
                    b.inline(c, plan, o, &args, s)?
                }
                _ => b.push_out(
                    Op::CallOut {
                        kind: CallKind::Method,
                        ip: ip as u32,
                        packed: ((name as u64) << 32) | ((obj as u64) << 16) | arg_base as u64,
                        argc,
                    },
                    Rep::Tagged,
                    Some(s),
                ),
            };
        }
        Instr::CallWithThis {
            dst,
            callee,
            this_v,
            arg_base,
            argc,
            ..
        } => {
            let s = fs.id(b, defs);
            defs[dst as usize] = match inline {
                Some((c, plan, Site::Call { fid })) => {
                    let cv = b.tagged(defs[callee as usize]);
                    b.push_out(Op::GuardFid { v: cv, fid }, Rep::Void, Some(s));
                    let this = b.tagged(defs[this_v as usize]);
                    let args: Vec<V> = (0..argc).map(|k| defs[(arg_base + k) as usize]).collect();
                    b.inline(c, plan, this, &args, s)?
                }
                _ => b.push_out(
                    Op::CallOut {
                        kind: CallKind::WithThis,
                        ip: ip as u32,
                        packed: ((this_v as u64) << 32) | ((callee as u64) << 16) | arg_base as u64,
                        argc,
                    },
                    Rep::Tagged,
                    Some(s),
                ),
            };
        }
        Instr::Jump { target } => return Some(Flow::Jump(target as usize)),
        Instr::JumpIfFalse { cond, target } | Instr::JumpIfTrue { cond, target } => {
            let t = b.truthy(defs[cond as usize], fs, defs);
            let jump = target as usize;
            return Some(if matches!(*ins, Instr::JumpIfTrue { .. }) {
                Flow::Branch { cond: t, t: jump, f: ip + 1 }
            } else {
                Flow::Branch { cond: t, t: ip + 1, f: jump }
            });
        }
        Instr::JumpIfNotLt { a, b: c, target } | Instr::JumpIfNotLe { a, b: c, target } => {
            let cmp = if matches!(*ins, Instr::JumpIfNotLt { .. }) {
                Cmp::Lt
            } else {
                Cmp::Le
            };
            let x = b.f64(defs[a as usize], fs, defs);
            let y = b.f64(defs[c as usize], fs, defs);
            let t = b.emit(Op::CmpF(cmp, x, y), Rep::Bool);
            return Some(Flow::Branch {
                cond: t,
                t: ip + 1,
                f: target as usize,
            });
        }
        Instr::Return { src } => return Some(Flow::Ret(defs[src as usize])),
        Instr::ReturnUndefined => return Some(Flow::Ret(b.konst(Op::ConstTagged(undefined_bits), Rep::Tagged))),
        _ => return None,
    }
    Some(Flow::Next)
}

/// The frame state of the op being built, made on its first guard (or one
/// given: an inlined body's guards all take its call's).
struct FsCtx<'a> {
    ip: u32,
    regs: &'a [u16],
    id: Option<u32>,
}

impl FsCtx<'_> {
    fn id(&mut self, b: &mut Builder, defs: &[V]) -> u32 {
        if let Some(id) = self.id {
            return id;
        }
        let id = b.states.len() as u32;
        b.states.push(FrameState {
            ip: self.ip,
            regs: self.regs.iter().map(|&r| (r, defs[r as usize])).collect(),
        });
        self.id = Some(id);
        id
    }
}

struct Builder {
    insts: Vec<Inst>,
    states: Vec<FrameState>,
    cur_block: B,
    /// The current block's conversions: (value, representation) -> value.
    memo: Vec<(V, Rep, V)>,
    /// The current block's values, in order.
    out: Vec<V>,
    /// The constants, made once each at the entry: (representation, bits,
    /// value). Made where the bytecode loads them, a loop's rebuilt them
    /// every iteration.
    consts: Vec<(Rep, u64, V)>,
    inlined: u32,
}

impl Builder {
    fn push(&mut self, op: Op, rep: Rep, fs: Option<u32>) -> V {
        let v = self.insts.len() as V;
        self.insts.push(Inst {
            op,
            rep,
            fs,
            block: self.cur_block,
        });
        v
    }

    fn push_out(&mut self, op: Op, rep: Rep, fs: Option<u32>) -> V {
        let v = self.push(op, rep, fs);
        self.out.push(v);
        v
    }

    fn emit(&mut self, op: Op, rep: Rep) -> V {
        self.push_out(op, rep, None)
    }

    /// The constant `op`, made once at the entry (`consts`).
    fn konst(&mut self, op: Op, rep: Rep) -> V {
        let bits = match op {
            Op::ConstI32(c) => c as u32 as u64,
            Op::ConstF64(x) => x.to_bits(),
            Op::ConstBool(b) => u64::from(b),
            Op::ConstTagged(t) => t,
            _ => unreachable!("not a constant: {op:?}"),
        };
        if let Some(&(_, _, v)) = self.consts.iter().find(|&&(r, b, _)| r == rep && b == bits) {
            return v;
        }
        let v = self.insts.len() as V;
        self.insts.push(Inst {
            op,
            rep,
            fs: None,
            block: 0,
        });
        self.consts.push((rep, bits, v));
        v
    }

    fn rep(&self, v: V) -> Rep {
        self.insts[v as usize].rep
    }

    fn memo_get(&self, v: V, r: Rep) -> Option<V> {
        self.memo.iter().find(|&&(a, b, _)| a == v && b == r).map(|&(_, _, c)| c)
    }

    /// The straight-line callee `c` inlined into the current block (see
    /// `inlinable`): `this` and the arguments its registers start with,
    /// every guard the call's frame state `call_fs`. Its result, in its
    /// own representation.
    fn inline(&mut self, c: &FuncProto, plan: &PropShapePlan, this: V, args: &[V], call_fs: u32) -> Option<V> {
        let undef = self.konst(Op::ConstTagged(Value::UNDEFINED.bits()), Rep::Tagged);
        let mut defs = vec![undef; c.reg_count as usize];
        defs[0] = this;
        for (k, &a) in args.iter().take(c.param_count as usize).enumerate() {
            defs[1 + k] = a;
        }
        let callee = Fn { p: c, plan };
        let mut fs = FsCtx {
            ip: 0,
            regs: &[],
            id: Some(call_fs),
        };
        for (ip, i) in c.code.iter().enumerate() {
            match translate(self, &callee, ip, i, &mut defs, &mut fs, None)? {
                Flow::Next => {}
                Flow::Ret(v) => {
                    self.inlined += 1;
                    return Some(v);
                }
                Flow::Jump(_) | Flow::Branch { .. } => return None,
            }
        }
        None
    }

    /// `v` as an f64; a boxed value is unboxed behind a guard.
    fn f64(&mut self, v: V, fs: &mut FsCtx, defs: &[V]) -> V {
        if let Op::ConstI32(c) = self.insts[v as usize].op {
            return self.konst(Op::ConstF64(c as f64), Rep::F64);
        }
        match self.rep(v) {
            Rep::F64 => v,
            r => {
                if let Some(c) = self.memo_get(v, Rep::F64) {
                    return c;
                }
                let c = match r {
                    Rep::I32 => self.emit(Op::I32ToF64(v), Rep::F64),
                    Rep::Tagged => {
                        let s = fs.id(self, defs);
                        self.push_out(Op::TaggedToF64(v), Rep::F64, Some(s))
                    }
                    // `true + 1` is a Number, but rare: the frame takes it.
                    _ => {
                        let t = self.tagged(v);
                        let s = fs.id(self, defs);
                        self.push_out(Op::TaggedToF64(t), Rep::F64, Some(s))
                    }
                };
                self.memo.push((v, Rep::F64, c));
                c
            }
        }
    }

    /// `v` as an i32 by ToInt32.
    fn i32(&mut self, v: V, fs: &mut FsCtx, defs: &[V]) -> V {
        if self.rep(v) == Rep::I32 {
            return v;
        }
        if let Op::ConstF64(x) = self.insts[v as usize].op {
            let c = Value::num(x);
            if c.is_int() {
                return self.konst(Op::ConstI32(c.as_int()), Rep::I32);
            }
        }
        if let Some(c) = self.memo_get(v, Rep::I32) {
            return c;
        }
        let x = self.f64(v, fs, defs);
        let s = fs.id(self, defs);
        let c = self.push_out(Op::F64ToI32(x), Rep::I32, Some(s));
        self.memo.push((v, Rep::I32, c));
        c
    }

    /// `v`'s truthiness as a Bool.
    fn truthy(&mut self, v: V, fs: &mut FsCtx, defs: &[V]) -> V {
        match self.rep(v) {
            Rep::Bool => v,
            Rep::I32 => self.emit(Op::I32Truthy(v), Rep::Bool),
            Rep::F64 => self.emit(Op::F64Truthy(v), Rep::Bool),
            _ => {
                let s = fs.id(self, defs);
                self.push_out(Op::TaggedTruthy(v), Rep::Bool, Some(s))
            }
        }
    }

    /// `v` boxed.
    fn tagged(&mut self, v: V) -> V {
        if self.rep(v) == Rep::Tagged {
            return v;
        }
        let boxed = match self.insts[v as usize].op {
            Op::ConstI32(c) => Some(Value::int(c)),
            Op::ConstF64(x) => Some(Value::num(x)),
            Op::ConstBool(b) => Some(Value::bool(b)),
            _ => None,
        };
        if let Some(c) = boxed {
            return self.konst(Op::ConstTagged(c.bits()), Rep::Tagged);
        }
        if let Some(c) = self.memo_get(v, Rep::Tagged) {
            return c;
        }
        let c = self.emit(Op::Box(v), Rep::Tagged);
        self.memo.push((v, Rep::Tagged, c));
        c
    }
}
