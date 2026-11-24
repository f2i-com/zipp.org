//! Frame-less execution of small acyclic Python leaf functions (`LeafBody`).
//!
//! Straight-line and forward-branching Python functions (arithmetic helpers,
//! default-argument wrappers, simple methods like `self.value += d; return
//! self.value`, conditional property setters like `if value < 0: raise ...;
//! self._cents = value`) are scanned once per `FuncProto` into [`LeafInfo`].
//! At a hot `PyCall` (both interpreter and compiled JIT regions) or property
//! accessor site, `Vm::py_leaf_exec` runs the body directly in a small stack
//! register file without pushing a `Frame`, resizing `Vm::regs`, or allocating
//! on the VM heap.
//!
//! ## Atomicity & GC safety
//!
//! * No instruction executed here allocates on the VM heap or invokes arbitrary
//!   guest code: `Vm::maybe_gc()` is never needed, stack registers cannot go
//!   stale, and JIT pinned pointers remain valid.
//! * Attribute stores (`PySetAttr`) are pre-validated against the callee's own
//!   layout-mode cache entries and buffered in a stack array, committed only
//!   when `Return` is reached. Any bail (overflow past 48-bit immediate int,
//!   non-numeric operand, cache miss, slow edge, or hitting a `raise`/call
//!   branch) leaves VM state completely untouched so the caller transparently
//!   falls back to pushing the real frame.

use super::super::py_ops::py_small_arith;
use super::super::py_rt::{ic, py_frame_guard_start, PyIc};
use super::super::*;
use crate::bytecode::{Instr, PyArithOp};
use crate::heap::HeapObj;
use crate::value::Value;

/// Maximum register index used in the main body of a leaf function (cold
/// slow-path blocks never execute here, so `proto.reg_count` may exceed this).
const LEAF_MAX_REGS: usize = 48;
/// Fast-path small register file size (covers Counter.bump, Base.scale, lcg,
/// balance getter/setter, lambdas — only 96 bytes on stack!).
const LEAF_SMALL_REGS: usize = 12;
/// Medium register file size (covers `Derived.scale` / `Checking.withdraw` ~24 regs).
const LEAF_MED_REGS: usize = 28;
/// Maximum parameter count of a leaf body.
const LEAF_MAX_PARAMS: usize = 12;
/// Maximum main-body instruction count scanned for a leaf body.
const LEAF_MAX_CODE: usize = 160;
/// Maximum buffered attribute writes per leaf call (shared across nested leaves;
/// 6 covers e.g. `Bank.transfer` -> `withdraw` (2) + `deposit` (2) + 2 counters).
const LEAF_MAX_WRITES: usize = 6;
/// Maximum nested leaf call depth (e.g. `Bank.transfer` -> `Checking.withdraw` -> `Account.withdraw` -> `balance.setter`).
const LEAF_MAX_DEPTH: u8 = 4;
/// Maximum total steps across a leaf call and its nested leaves.
const LEAF_MAX_STEPS: i64 = 128;
/// High bit of `PendingAttr::next_layout` marking an `ATTR_APPEND_L`.
const PENDING_APPEND_BIT: u32 = 0x8000_0000;

env_off_switch! {
    /// Whether leaf bodies run without pushing a frame (`Vm::py_leaf_exec`).
    /// On unless `ZIPP_NO_PY_LEAF_CALLS` is set. Read once per process.
    fn py_leaf_calls_enabled() = "ZIPP_NO_PY_LEAF_CALLS"
}

/// Cached analysis result for one `FuncProto`.
#[derive(Clone, Copy, Default)]
pub(crate) enum LeafSlot {
    #[default]
    Unread,
    No,
    Yes(LeafInfo),
}

/// Static facts about an acyclic leaf `FuncProto`.
#[derive(Clone, Copy)]
pub(crate) struct LeafInfo {
    pub main_end: u16,
    pub params: u8,
    /// Number of register slots needed (`1 + highest_reg_used`), `<= LEAF_MAX_REGS`.
    pub reg_slots: u8,
    /// True when the function has no unsupported calls outside raise-only
    /// branches, making call sites targeting it eligible for `py_call_site_native`.
    pub native_eligible: bool,
}

/// Compact 16-byte buffered attribute write (`is_append` packed in bit 31 of `layout_flags`).
#[derive(Clone, Copy)]
struct PendingAttr {
    dict_idx: u32,
    pos: u32,
    layout_flags: u32,
    val: Value,
}

impl PendingAttr {
    const EMPTY: Self = Self {
        dict_idx: 0,
        pos: 0,
        layout_flags: 0,
        val: Value::UNDEFINED,
    };

    #[inline(always)]
    fn is_append(&self) -> bool {
        (self.layout_flags & PENDING_APPEND_BIT) != 0
    }

    #[inline(always)]
    fn next_layout(&self) -> u32 {
        self.layout_flags & !PENDING_APPEND_BIT
    }
}

enum LeafAttrGet {
    Slot { dict_idx: u32, pos: u32, val: Value },
    ClassVal { dict_idx: u32, val: Value },
}

enum LeafSetKind {
    Layout { dict_idx: u32, pos: u32, next_layout: u32, is_append: bool },
    PropSetter { fid: u32, fv: Value },
}

/// Shared commit buffer across a top-level leaf call and its nested leaf calls.
struct LeafState {
    pending: [PendingAttr; LEAF_MAX_WRITES],
    n_pending: usize,
    pending_mself: Option<(u32, usize)>,
    steps: i64,
    /// Single-entry `(obj_bits, cls, dict, dict_layout, dict_len)` cache valid
    /// for the entire leaf call (leaf execution never mutates `cls`/`dict` slots
    /// or runs guest code).
    last_obj: u64,
    last_cls: Value,
    last_dict: Value,
    last_dict_layout: u32,
    last_dict_len: u32,
    /// Single-entry verified `(cls_bits, cls_hver, cls_ver)` stamp cache.
    stamp_cls: u64,
    stamp_hver: u32,
    stamp_ver: u64,
}

impl LeafState {
    #[inline(always)]
    fn new() -> Self {
        Self {
            pending: [PendingAttr::EMPTY; LEAF_MAX_WRITES],
            n_pending: 0,
            pending_mself: None,
            steps: 0,
            last_obj: 0,
            last_cls: Value::UNDEFINED,
            last_dict: Value::UNDEFINED,
            last_dict_layout: u32::MAX,
            last_dict_len: 0,
            stamp_cls: 0,
            stamp_hver: 0,
            stamp_ver: 0,
        }
    }
}

/// Scan a `FuncProto` once to see if its main body (before frame guard / cold
/// blocks) is an acyclic leaf function with at least one non-allocating path
/// to `Return`.
fn py_leaf_scan(p: &crate::bytecode::FuncProto) -> Option<LeafInfo> {
    if !py_leaf_calls_enabled()
        || p.is_generator
        || p.is_async
        || !p.is_strict
        || p.lexical_this
        || p.rest_reg.is_some()
        || p.arguments_reg.is_some()
        || (p.param_count as usize) > LEAF_MAX_PARAMS
    {
        return None;
    }
    let code = p.code.as_slice();
    if code.is_empty() {
        return None;
    }
    let main_end = py_frame_guard_start(p)
        .map(|s| (s as usize).min(code.len()))
        .unwrap_or(code.len());
    if main_end == 0 || main_end > LEAF_MAX_CODE {
        return None;
    }

    // Reject if any jump inside the main body targets `<= ip` (has a loop).
    for (ip, instr) in code[..main_end].iter().enumerate() {
        let target = match *instr {
            Instr::Jump { target }
            | Instr::JumpIfFalse { target, .. }
            | Instr::JumpIfTrue { target, .. }
            | Instr::JumpIfNotLt { target, .. }
            | Instr::JumpIfNotLe { target, .. }
            | Instr::PyJumpCompare { target, .. } => Some(target as usize),
            Instr::Yield { .. }
            | Instr::YieldDelegate { .. } => return None,
            _ => None,
        };
        if let Some(t) = target {
            if t <= ip {
                return None;
            }
        }
    }

    // Forward reachability over leaf-supported ops: prove at least one
    // Return is reachable from ip 0 without passing through an unsupported op.
    let mut reach = [false; LEAF_MAX_CODE];
    reach[0] = true;
    let mut has_return = false;
    let mut has_js_call = false;
    let mut reg_oob = false;
    let mut max_reg = p.param_count as usize;
    let mut chk = |r: u16| {
        let ru = r as usize;
        if ru >= LEAF_MAX_REGS {
            reg_oob = true;
        } else if ru > max_reg {
            max_reg = ru;
        }
    };

    for (ip, instr) in code[..main_end].iter().enumerate() {
        if matches!(*instr, Instr::Call { .. } | Instr::CallMethod { .. }) {
            has_js_call = true;
        }
        if !reach[ip] {
            continue;
        }
        let mut mark = |next: usize| {
            if next < main_end {
                reach[next] = true;
            }
        };
        match *instr {
            Instr::LoadGlobal { dst, .. }
            | Instr::LoadNull { dst }
            | Instr::LoadUndefined { dst }
            | Instr::LoadBool { dst, .. }
            | Instr::LoadInt { dst, .. }
            | Instr::LoadConst { dst, .. } => {
                chk(dst);
                mark(ip + 1);
            }
            Instr::Move { dst, src } => {
                chk(dst);
                chk(src);
                mark(ip + 1);
            }
            Instr::GetProp { dst, obj, .. } => {
                chk(dst);
                chk(obj);
                mark(ip + 1);
            }
            Instr::GetIndex { dst, obj, key } => {
                chk(dst);
                chk(obj);
                chk(key);
                mark(ip + 1);
            }
            Instr::TypeOfIs { dst, a, .. }
            | Instr::Not { dst, a }
            | Instr::Neg { dst, a }
            | Instr::BitNot { dst, a }
            | Instr::AddInt { dst, a, .. }
            | Instr::PyAddImm { dst, a, .. } => {
                chk(dst);
                chk(a);
                mark(ip + 1);
            }
            Instr::Eq { dst, a, b }
            | Instr::Ne { dst, a, b }
            | Instr::PyArith { dst, a, b, .. }
            | Instr::PyCompare { dst, a, b, .. } => {
                chk(dst);
                chk(a);
                chk(b);
                mark(ip + 1);
            }
            Instr::PyGetAttr { dst, obj, .. } | Instr::PyClassAttr { dst, obj, .. } => {
                chk(dst);
                chk(obj);
                mark(ip + 1);
            }
            Instr::PyMethod { dst, obj, rt, .. } => {
                chk(dst);
                chk(obj);
                chk(rt);
                mark(ip + 1);
            }
            Instr::PushHandler { .. } | Instr::PopHandler => {
                mark(ip + 1);
            }
            Instr::PySetAttr { obj, val, .. } => {
                chk(obj);
                chk(val);
                mark(ip + 1);
            }
            Instr::PyGetItem { dst, o, k, seq, dict, .. } => {
                chk(dst);
                chk(o);
                chk(k);
                chk(seq);
                chk(dict);
                mark(ip + 1);
            }
            Instr::PyLen { dst, v, rt, .. } | Instr::PyTruth { dst, v, rt, .. } => {
                chk(dst);
                chk(v);
                chk(rt);
                mark(ip + 1);
            }
            Instr::CallWithThis { dst, callee, this_v, arg_base, argc, .. } if argc == 3 => {
                chk(dst);
                chk(callee);
                chk(this_v);
                chk(arg_base);
                chk(arg_base + 1);
                chk(arg_base + 2);
                mark(ip + 1);
            }
            Instr::PyCall { dst, f, arg_base, argc, .. } if (argc as usize) <= LEAF_MAX_PARAMS => {
                chk(dst);
                chk(f);
                for k in 0..argc {
                    chk(arg_base + k);
                }
                mark(ip + 1);
            }
            Instr::LoadBigInt { dst, value } if Value::small_bigint(value.get()).is_some() => {
                chk(dst);
                mark(ip + 1);
            }
            Instr::Jump { target } => mark(target as usize),
            Instr::JumpIfFalse { cond, target } | Instr::JumpIfTrue { cond, target } => {
                chk(cond);
                mark(ip + 1);
                mark(target as usize);
            }
            Instr::JumpIfNotLt { a, b, target }
            | Instr::JumpIfNotLe { a, b, target }
            | Instr::PyJumpCompare { a, b, target, .. } => {
                chk(a);
                chk(b);
                mark(ip + 1);
                mark(target as usize);
            }
            Instr::Return { src } => {
                chk(src);
                has_return = true;
            }
            _ => {}
        }
    }

    if !has_return || reg_oob {
        return None;
    }

    // Check if any unsupported JS call sits on a non-raising path from ip 0
    // that can reach a Return (ignoring unreachable `except:` handler bodies
    // and `if ...: raise ...` branches, including `R.raise` CallMethod).
    let native_eligible = if !has_js_call {
        true
    } else {
        let is_raise_call = |i: &Instr| -> bool {
            matches!(
                *i,
                Instr::CallMethod { name, .. }
                    if p.string_constants.get(name as usize).is_some_and(|s| s == "raise")
            )
        };
        let mut cfg_reach = [false; LEAF_MAX_CODE];
        cfg_reach[0] = true;
        for ip in 0..main_end {
            if !cfg_reach[ip] {
                continue;
            }
            let mut mk = |next: usize| {
                if next < main_end {
                    cfg_reach[next] = true;
                }
            };
            match code[ip] {
                Instr::Return { .. } | Instr::PyRaise { .. } | Instr::Throw { .. } => {}
                ref i if is_raise_call(i) => {}
                Instr::Jump { target } => mk(target as usize),
                Instr::JumpIfFalse { target, .. }
                | Instr::JumpIfTrue { target, .. }
                | Instr::JumpIfNotLt { target, .. }
                | Instr::JumpIfNotLe { target, .. }
                | Instr::PyJumpCompare { target, .. } => {
                    mk(ip + 1);
                    mk(target as usize);
                }
                _ => mk(ip + 1),
            }
        }
        let mut can_return = [false; LEAF_MAX_CODE];
        for ip in (0..main_end).rev() {
            can_return[ip] = match code[ip] {
                Instr::Return { .. } => true,
                Instr::PyRaise { .. } | Instr::Throw { .. } => false,
                ref i if is_raise_call(i) => false,
                Instr::Jump { target } => (target as usize) < main_end && can_return[target as usize],
                Instr::JumpIfFalse { target, .. }
                | Instr::JumpIfTrue { target, .. }
                | Instr::JumpIfNotLt { target, .. }
                | Instr::JumpIfNotLe { target, .. }
                | Instr::PyJumpCompare { target, .. } => {
                    (ip + 1 < main_end && can_return[ip + 1])
                        || ((target as usize) < main_end && can_return[target as usize])
                }
                _ => ip + 1 < main_end && can_return[ip + 1],
            };
        }
        !code[..main_end].iter().enumerate().any(|(ip, i)| {
            cfg_reach[ip]
                && can_return[ip]
                && matches!(*i, Instr::Call { .. } | Instr::CallMethod { .. })
        })
    };

    Some(LeafInfo {
        main_end: main_end as u16,
        params: p.param_count as u8,
        reg_slots: (max_reg + 1) as u8,
        native_eligible,
    })
}

#[inline(always)]
fn py_i128_arith(op: PyArithOp, x: i128, y: i128) -> Option<i128> {
    match op {
        PyArithOp::Add => x.checked_add(y),
        PyArithOp::Sub => x.checked_sub(y),
        PyArithOp::Mul => x.checked_mul(y),
        PyArithOp::FloorDiv => {
            if y == 0 {
                return None;
            }
            let q = x.checked_div(y)?;
            let r = x.checked_rem(y)?;
            Some(if r != 0 && (r ^ y) < 0 { q.checked_sub(1)? } else { q })
        }
        PyArithOp::Mod => {
            if y == 0 {
                return None;
            }
            let r = x.checked_rem(y)?;
            Some(if r != 0 && (r ^ y) < 0 { r.checked_add(y)? } else { r })
        }
        PyArithOp::BitAnd => Some(x & y),
        PyArithOp::BitOr => Some(x | y),
        PyArithOp::BitXor => Some(x ^ y),
        _ => None,
    }
}

#[inline(always)]
fn leaf_arith_num(op: PyArithOp, va: Value, vb: Value) -> Option<Value> {
    Some(match op {
        PyArithOp::Add => {
            if va.is_int() && vb.is_int() {
                match va.as_int().checked_add(vb.as_int()) {
                    Some(v) => Value::int(v),
                    None => Value::num(va.as_int() as f64 + vb.as_int() as f64),
                }
            } else {
                Value::num(va.as_f64() + vb.as_f64())
            }
        }
        PyArithOp::Sub => {
            if va.is_int() && vb.is_int() {
                match va.as_int().checked_sub(vb.as_int()) {
                    Some(v) => Value::int(v),
                    None => Value::num(va.as_int() as f64 - vb.as_int() as f64),
                }
            } else {
                Value::num(va.as_f64() - vb.as_f64())
            }
        }
        PyArithOp::Mul => {
            if va.is_int() && vb.is_int() {
                let (ia, ib) = (va.as_int(), vb.as_int());
                match ia.checked_mul(ib) {
                    Some(v) if v != 0 || (ia | ib) >= 0 => Value::int(v),
                    _ => Value::num(ia as f64 * ib as f64),
                }
            } else {
                Value::num(va.as_f64() * vb.as_f64())
            }
        }
        PyArithOp::TrueDiv => {
            let y = vb.as_f64();
            if y == 0.0 {
                return None;
            }
            Value::num(va.as_f64() / y)
        }
        _ => return None,
    })
}

/// Static facts about a pure 1-argument key lambda that ends by constructing a
/// small 1..=4-element tuple (`NewArray` + `PySeq(tuple=true)` + `Return`), so
/// `PYORD` op 4 can evaluate its tuple elements directly into `OrdKey::Seq`
/// without allocating any `Array` or `tuple` Object.
#[derive(Clone, Copy)]
pub(crate) struct TupleKeyInfo {
    pub new_arr_ip: u16,
    pub arg_base: u16,
    pub argc: u8,
    pub reg_slots: u8,
}

/// Scan a 1-argument `FuncProto` to see if it is a pure side-effect-free key
/// lambda ending in `[NewArray(1..=4), PySeq(tuple=true), Return]`.
pub(crate) fn py_leaf_tuple_key_scan(p: &crate::bytecode::FuncProto) -> Option<TupleKeyInfo> {
    if !py_leaf_calls_enabled()
        || p.is_generator
        || p.is_async
        || !p.is_strict
        || p.lexical_this
        || p.rest_reg.is_some()
        || p.arguments_reg.is_some()
        || p.param_count != 1
    {
        return None;
    }
    let code = p.code.as_slice();
    let main_end = py_frame_guard_start(p)
        .map(|s| (s as usize).min(code.len()))
        .unwrap_or(code.len());
    if main_end < 3 || main_end > LEAF_MAX_CODE {
        return None;
    }
    // Find trailing `[NewArray, PySeq(tuple=true), Return]` inside `code[..main_end]`.
    let mut found = None;
    for k in (0..=main_end - 3).rev() {
        if let (
            Instr::NewArray { dst: a, arg_base, argc },
            Instr::PySeq { dst: s, items: a2, tuple: true, .. },
            Instr::Return { src: s2 },
        ) = (code[k], code[k + 1], code[k + 2])
        {
            if a == a2 && s == s2 && (1..=4).contains(&argc) {
                found = Some((k, arg_base, argc));
                break;
            }
        }
    }
    let (k, arg_base, argc) = found?;
    for (ip, instr) in code[..k].iter().enumerate() {
        let target = match *instr {
            Instr::Jump { target }
            | Instr::JumpIfFalse { target, .. }
            | Instr::JumpIfTrue { target, .. }
            | Instr::JumpIfNotLt { target, .. }
            | Instr::JumpIfNotLe { target, .. }
            | Instr::PyJumpCompare { target, .. } => Some(target as usize),
            Instr::PushHandler { .. }
            | Instr::PushFinally { .. }
            | Instr::Yield { .. }
            | Instr::YieldDelegate { .. } => return None,
            _ => None,
        };
        if let Some(t) = target {
            if t <= ip {
                return None;
            }
        }
    }
    let mut reach = [false; LEAF_MAX_CODE];
    reach[0] = true;
    let mut reg_oob = false;
    let mut max_reg = (arg_base + argc).saturating_sub(1) as usize;
    if max_reg >= LEAF_MAX_REGS {
        return None;
    }
    let mut chk = |r: u16| {
        let ru = r as usize;
        if ru >= LEAF_MAX_REGS {
            reg_oob = true;
        } else if ru > max_reg {
            max_reg = ru;
        }
    };
    for (ip, instr) in code[..k].iter().enumerate() {
        if !reach[ip] {
            continue;
        }
        let mut mark = |next: usize| {
            if next <= k {
                reach[next] = true;
            }
        };
        match *instr {
            Instr::LoadGlobal { dst, .. }
            | Instr::LoadNull { dst }
            | Instr::LoadUndefined { dst }
            | Instr::LoadBool { dst, .. }
            | Instr::LoadInt { dst, .. }
            | Instr::LoadConst { dst, .. } => {
                chk(dst);
                mark(ip + 1);
            }
            Instr::Move { dst, src } => {
                chk(dst);
                chk(src);
                mark(ip + 1);
            }
            Instr::GetProp { dst, obj, .. } => {
                chk(dst);
                chk(obj);
                mark(ip + 1);
            }
            Instr::GetIndex { dst, obj, key } => {
                chk(dst);
                chk(obj);
                chk(key);
                mark(ip + 1);
            }
            Instr::TypeOfIs { dst, a, .. }
            | Instr::Not { dst, a }
            | Instr::Neg { dst, a }
            | Instr::BitNot { dst, a }
            | Instr::AddInt { dst, a, .. }
            | Instr::PyAddImm { dst, a, .. } => {
                chk(dst);
                chk(a);
                mark(ip + 1);
            }
            Instr::Eq { dst, a, b }
            | Instr::Ne { dst, a, b }
            | Instr::PyArith { dst, a, b, .. }
            | Instr::PyCompare { dst, a, b, .. } => {
                chk(dst);
                chk(a);
                chk(b);
                mark(ip + 1);
            }
            Instr::PyGetAttr { dst, obj, .. } | Instr::PyClassAttr { dst, obj, .. } => {
                chk(dst);
                chk(obj);
                mark(ip + 1);
            }
            Instr::PyGetItem { dst, o, k: kv, seq, dict, .. } => {
                chk(dst);
                chk(o);
                chk(kv);
                chk(seq);
                chk(dict);
                mark(ip + 1);
            }
            Instr::PyLen { dst, v, rt, .. } | Instr::PyTruth { dst, v, rt, .. } => {
                chk(dst);
                chk(v);
                chk(rt);
                mark(ip + 1);
            }
            Instr::LoadBigInt { dst, value } if Value::small_bigint(value.get()).is_some() => {
                chk(dst);
                mark(ip + 1);
            }
            Instr::Jump { target } => mark(target as usize),
            Instr::JumpIfFalse { cond, target } | Instr::JumpIfTrue { cond, target } => {
                chk(cond);
                mark(ip + 1);
                mark(target as usize);
            }
            Instr::JumpIfNotLt { a, b, target }
            | Instr::JumpIfNotLe { a, b, target }
            | Instr::PyJumpCompare { a, b, target, .. } => {
                chk(a);
                chk(b);
                mark(ip + 1);
                mark(target as usize);
            }
            // Any Return or mutating/calling op cannot continue through `k`.
            _ => {}
        }
    }
    if !reach[k] || reg_oob {
        return None;
    }
    Some(TupleKeyInfo {
        new_arr_ip: k as u16,
        arg_base,
        argc: argc as u8,
        reg_slots: (max_reg + 1) as u8,
    })
}

/// True when a leaf `FuncProto` has 1 parameter and zero side-effecting ops
/// (`PySetAttr`, `CallWithThis`, `PyCall`, `PushHandler`, `PopHandler`, `SetProp`,
/// `SetIndex`) in its main body, making it safe to abort midway in `PYORD` op 4.
pub(crate) fn py_leaf_is_pure_1arg(p: &crate::bytecode::FuncProto, info: LeafInfo) -> bool {
    if info.params != 1 {
        return false;
    }
    let end = (info.main_end as usize).min(p.code.len());
    !p.code[..end].iter().any(|i| {
        matches!(
            i,
            Instr::PySetAttr { .. }
                | Instr::CallWithThis { .. }
                | Instr::PyCall { .. }
                | Instr::PushHandler { .. }
                | Instr::PopHandler
                | Instr::SetProp { .. }
                | Instr::SetIndex { .. }
        )
    })
}

impl<'p> Vm<'p> {
    /// Execute a scanned pure 1-arg tuple-key lambda (`TupleKeyInfo`) on `item`
    /// without allocating an `Array` or `tuple` Object, writing the `argc`
    /// element values into `out[..argc]`.
    #[inline]
    pub(crate) fn py_leaf_exec_tuple_key(
        &mut self,
        fid: u32,
        fv: Value,
        tk: TupleKeyInfo,
        item: Value,
        out: &mut [Value; 4],
    ) -> Option<usize> {
        let slots = tk.reg_slots as usize;
        if slots <= LEAF_SMALL_REGS {
            let mut regs = [Value::UNDEFINED; LEAF_SMALL_REGS];
            regs[0] = fv;
            regs[1] = item;
            self.py_leaf_run_tuple_key(fid, tk, &mut regs, out)
        } else if slots <= LEAF_MED_REGS {
            let mut regs = [Value::UNDEFINED; LEAF_MED_REGS];
            regs[0] = fv;
            regs[1] = item;
            self.py_leaf_run_tuple_key(fid, tk, &mut regs, out)
        } else {
            let mut regs = [Value::UNDEFINED; LEAF_MAX_REGS];
            regs[0] = fv;
            regs[1] = item;
            self.py_leaf_run_tuple_key(fid, tk, &mut regs, out)
        }
    }

    fn py_leaf_run_tuple_key(
        &mut self,
        fid: u32,
        tk: TupleKeyInfo,
        regs: &mut [Value],
        out: &mut [Value; 4],
    ) -> Option<usize> {
        let stop_ip = tk.new_arr_ip as usize;
        let mut ip = 0usize;
        let mut steps = 0usize;
        while ip < stop_ip {
            steps += 1;
            if steps > 64 {
                return None;
            }
            let instr = self.func(fid as usize).code[ip];
            match instr {
                Instr::LoadGlobal { dst, idx } => {
                    let v = *self.globals.get(idx as usize)?;
                    if v == Value::UNINITIALIZED {
                        return None;
                    }
                    regs[dst as usize] = v;
                    ip += 1;
                }
                Instr::Move { dst, src } => {
                    regs[dst as usize] = regs[src as usize];
                    ip += 1;
                }
                Instr::LoadNull { dst } => {
                    regs[dst as usize] = Value::NULL;
                    ip += 1;
                }
                Instr::LoadUndefined { dst } => {
                    regs[dst as usize] = Value::UNDEFINED;
                    ip += 1;
                }
                Instr::LoadBool { dst, val } => {
                    regs[dst as usize] = Value::bool(val);
                    ip += 1;
                }
                Instr::LoadInt { dst, val } => {
                    regs[dst as usize] = Value::int(val);
                    ip += 1;
                }
                Instr::LoadBigInt { dst, value } => {
                    regs[dst as usize] = Value::small_bigint(value.get())?;
                    ip += 1;
                }
                Instr::LoadConst { dst, idx } => {
                    let raw = *self.func(fid as usize).constants.get(idx as usize)?;
                    let v = if raw.is_heap() && (raw.heap_index() & crate::vm::helpers_misc::STRING_CONST_BIT) != 0 {
                        let cached = self
                            .const_string_dense
                            .get(fid as usize)
                            .and_then(|f| f.get(idx as usize))
                            .copied()?;
                        if cached.is_undefined() {
                            return None;
                        }
                        cached
                    } else {
                        raw
                    };
                    regs[dst as usize] = v;
                    ip += 1;
                }
                Instr::GetProp { dst, obj, name } => {
                    let ov = regs[obj as usize];
                    if !ov.is_heap() {
                        return None;
                    }
                    let key = self.func(fid as usize).string_constants.get(name as usize)?.as_str();
                    regs[dst as usize] = self.py_leaf_get_prop(ov, key, false)?;
                    ip += 1;
                }
                Instr::GetIndex { dst, obj, key } => {
                    let (ov, kv) = (regs[obj as usize], regs[key as usize]);
                    if !ov.is_heap() || !kv.is_int() || kv.as_int() < 0 {
                        return None;
                    }
                    let oi = ov.heap_index();
                    if !self.array_js_len.is_empty() && self.array_js_len.contains_key(&oi) {
                        return None;
                    }
                    let HeapObj::Array(items) = self.heap.get(oi) else {
                        return None;
                    };
                    let v = *items.get(kv.as_int() as usize)?;
                    if v == Value::HOLE {
                        return None;
                    }
                    regs[dst as usize] = v;
                    ip += 1;
                }
                Instr::TypeOfIs { dst, a, code, neg } => {
                    let eq = if let Some(&want) = crate::bytecode::TYPEOF_NAMES.get(code as usize) {
                        let v = regs[a as usize];
                        let got = if v.is_undefined() {
                            "undefined"
                        } else if v.is_small_bigint() {
                            "bigint"
                        } else if v.is_number() {
                            "number"
                        } else if v == Value::TRUE || v == Value::FALSE {
                            "boolean"
                        } else {
                            self.type_of(v)
                        };
                        want == got
                    } else {
                        false
                    };
                    regs[dst as usize] = Value::bool(eq != neg);
                    ip += 1;
                }
                Instr::Not { dst, a } => {
                    let v = regs[a as usize];
                    if v == Value::TRUE {
                        regs[dst as usize] = Value::FALSE;
                    } else if v == Value::FALSE {
                        regs[dst as usize] = Value::TRUE;
                    } else {
                        return None;
                    }
                    ip += 1;
                }
                Instr::Neg { dst, a } => {
                    let v = regs[a as usize];
                    if let Some(n) = v.small_bigint_val() {
                        regs[dst as usize] = (n as i128).checked_neg().and_then(Value::small_bigint)?;
                    } else if v.is_number() {
                        regs[dst as usize] = Value::num(-v.as_f64());
                    } else {
                        return None;
                    }
                    ip += 1;
                }
                Instr::BitNot { dst, a } => {
                    let n = regs[a as usize].small_bigint_val()?;
                    regs[dst as usize] = Value::small_bigint(!(n as i128))?;
                    ip += 1;
                }
                Instr::AddInt { dst, a, imm, upd } => {
                    let va = regs[a as usize];
                    regs[dst as usize] = if va.is_int() {
                        match va.as_int().checked_add(imm) {
                            Some(v) => Value::int(v),
                            None => Value::num(va.as_int() as f64 + imm as f64),
                        }
                    } else if va.is_double() {
                        Value::num(va.as_f64() + imm as f64)
                    } else if upd {
                        Value::small_bigint((va.small_bigint_val()? as i128) + (imm as i128))?
                    } else {
                        return None;
                    };
                    ip += 1;
                }
                Instr::Eq { dst, a, b } | Instr::Ne { dst, a, b } => {
                    let (va, vb) = (regs[a as usize], regs[b as usize]);
                    let eq = if va.bits() == vb.bits() {
                        !(va.is_double() && va.as_f64().is_nan())
                    } else if va.is_number() && vb.is_number() {
                        va.as_f64() == vb.as_f64()
                    } else if (va == Value::NULL || va == Value::TRUE || va == Value::FALSE || va.is_small_bigint())
                        && (vb == Value::NULL || vb == Value::TRUE || vb == Value::FALSE || vb.is_small_bigint())
                    {
                        false
                    } else {
                        return None;
                    };
                    regs[dst as usize] = Value::bool(eq != matches!(instr, Instr::Ne { .. }));
                    ip += 1;
                }
                Instr::PyAddImm { dst, a, imm, .. } => {
                    let va = regs[a as usize];
                    regs[dst as usize] = if let Some(x) = va.small_bigint_val() {
                        Value::small_bigint((x as i128) + (imm as i128))?
                    } else if va.is_number() {
                        Value::num(va.as_f64() + imm as f64)
                    } else {
                        return None;
                    };
                    ip += 1;
                }
                Instr::PyArith { op, dst, a, b, .. } => {
                    let (va, vb) = (regs[a as usize], regs[b as usize]);
                    regs[dst as usize] = if let (Some(x), Some(y)) = (va.small_bigint_val(), vb.small_bigint_val()) {
                        py_small_arith(op, x, y).and_then(|r| Value::small_bigint(r as i128))?
                    } else if va.is_number() && vb.is_number() {
                        leaf_arith_num(op, va, vb)?
                    } else {
                        return None;
                    };
                    ip += 1;
                }
                Instr::PyCompare { op, dst, a, b, .. } => {
                    let r = self.py_compare(op, regs[a as usize], regs[b as usize]).ok()??;
                    regs[dst as usize] = Value::bool(r);
                    ip += 1;
                }
                Instr::PyJumpCompare { op, a, b, when, target, .. } => {
                    let r = self.py_compare(op, regs[a as usize], regs[b as usize]).ok()??;
                    ip = if r == when { target as usize } else { ip + 1 };
                }
                Instr::Jump { target } => ip = target as usize,
                Instr::JumpIfFalse { cond, target } => {
                    let v = regs[cond as usize];
                    if v == Value::FALSE {
                        ip = target as usize;
                    } else if v == Value::TRUE {
                        ip += 1;
                    } else {
                        return None;
                    }
                }
                Instr::JumpIfTrue { cond, target } => {
                    let v = regs[cond as usize];
                    if v == Value::TRUE {
                        ip = target as usize;
                    } else if v == Value::FALSE {
                        ip += 1;
                    } else {
                        return None;
                    }
                }
                Instr::JumpIfNotLt { a, b, target } => {
                    let (va, vb) = (regs[a as usize], regs[b as usize]);
                    let lt = if let (Some(x), Some(y)) = (va.small_bigint_val(), vb.small_bigint_val()) {
                        x < y
                    } else if va.is_number() && vb.is_number() {
                        va.as_f64() < vb.as_f64()
                    } else {
                        return None;
                    };
                    ip = if !lt { target as usize } else { ip + 1 };
                }
                Instr::JumpIfNotLe { a, b, target } => {
                    let (va, vb) = (regs[a as usize], regs[b as usize]);
                    let le = if let (Some(x), Some(y)) = (va.small_bigint_val(), vb.small_bigint_val()) {
                        x <= y
                    } else if va.is_number() && vb.is_number() {
                        va.as_f64() <= vb.as_f64()
                    } else {
                        return None;
                    };
                    ip = if !le { target as usize } else { ip + 1 };
                }
                Instr::PyGetAttr { dst, obj, .. } => {
                    let mut tmp = LeafState::new();
                    let v = match self.py_leaf_attr_get(fid, ip, regs[obj as usize], &mut tmp)? {
                        LeafAttrGet::Slot { val, .. } | LeafAttrGet::ClassVal { val, .. } => val,
                    };
                    regs[dst as usize] = v;
                    ip += 1;
                }
                Instr::PyClassAttr { dst, obj, .. } => {
                    let mut tmp = LeafState::new();
                    let LeafAttrGet::ClassVal { val, .. } = self.py_leaf_class_attr_get(fid, ip, regs[obj as usize], &mut tmp)? else {
                        return None;
                    };
                    regs[dst as usize] = val;
                    ip += 1;
                }
                Instr::PyGetItem { dst, o, k, seq, dict, .. } => {
                    let Some(Some(v)) = self
                        .py_get_item(regs[o as usize], regs[k as usize], regs[seq as usize], regs[dict as usize])
                        .ok()?
                    else {
                        return None;
                    };
                    regs[dst as usize] = v;
                    ip += 1;
                }
                Instr::PyLen { dst, v, rt, .. } => {
                    let n = self.py_len(regs[v as usize], regs[rt as usize])?;
                    regs[dst as usize] = Value::small_bigint(n as i128)?;
                    ip += 1;
                }
                Instr::PyTruth { dst, v, rt, .. } => {
                    let b = self.py_truth(regs[v as usize], regs[rt as usize])?;
                    regs[dst as usize] = Value::bool(b);
                    ip += 1;
                }
                _ => return None,
            }
        }
        if ip != stop_ip {
            return None;
        }
        let n = tk.argc as usize;
        let ab = tk.arg_base as usize;
        out[..n].copy_from_slice(&regs[ab..ab + n]);
        Some(n)
    }

    /// Execute a verified pure 1-arg leaf key function (zero side effects).
    #[inline]
    pub(crate) fn py_leaf_exec_pure_1arg(&mut self, fid: u32, fv: Value, info: LeafInfo, item: Value) -> Option<Value> {
        let mut st = LeafState::new();
        let ret = self.py_leaf_dispatch(fid, fv, info, &[item], &mut st, 0)?;
        if st.n_pending != 0 || st.pending_mself.is_some() {
            return None;
        }
        Some(ret)
    }

    /// Cached [`LeafInfo`] for `fid`, scanned once per code object.
    #[inline]
    pub(crate) fn py_leaf_info(&mut self, fid: u32) -> Option<LeafInfo> {
        let f = fid as usize;
        let known = self.py_rt.as_deref()?.leaf_bodies.get(f).copied().unwrap_or(LeafSlot::Unread);
        match known {
            LeafSlot::Yes(info) => Some(info),
            LeafSlot::No => None,
            LeafSlot::Unread => {
                let scanned = py_leaf_scan(self.func(f));
                let slot = match scanned {
                    Some(info) => LeafSlot::Yes(info),
                    None => LeafSlot::No,
                };
                if let Some(p) = self.py_rt.as_deref_mut() {
                    if p.leaf_bodies.len() <= f {
                        p.leaf_bodies.resize(f + 1, LeafSlot::Unread);
                    }
                    p.leaf_bodies[f] = slot;
                }
                scanned
            }
        }
    }

    /// Resolve `(cls, dict)` for `obj` using `st`'s single-entry receiver cache.
    #[inline(always)]
    fn py_leaf_parts(&self, k: (u32, u32, usize), obj: Value, st: &mut LeafState) -> Option<(Value, Value)> {
        let ob = obj.bits();
        if ob == st.last_obj {
            return Some((st.last_cls, st.last_dict));
        }
        if !obj.is_heap() {
            return None;
        }
        let (cls, dict) = self.py_hit_parts(k, obj)?;
        if !cls.is_heap() || !dict.is_heap() {
            return None;
        }
        st.last_obj = ob;
        st.last_cls = cls;
        st.last_dict = dict;
        st.last_dict_layout = u32::MAX;
        st.last_dict_len = 0;
        Some((cls, dict))
    }

    /// Cached `(layout, len)` lookup for `dict` when it matches `st.last_dict`.
    #[inline(always)]
    fn py_leaf_attrs_of(&self, dict: Value, st: &mut LeafState) -> Option<(u32, usize)> {
        if dict.bits() == st.last_dict.bits() && st.last_dict_layout != u32::MAX {
            return Some((st.last_dict_layout, st.last_dict_len as usize));
        }
        let (l, n) = self.py_attrs_of(dict)?;
        if dict.bits() == st.last_dict.bits() {
            st.last_dict_layout = l;
            st.last_dict_len = n as u32;
        }
        Some((l, n))
    }

    /// Verify `cls`'s `(hver, ver)` stamp using `st`'s single-entry stamp cache.
    #[inline(always)]
    fn py_leaf_stamp(&self, k: (u32, u32, usize), cls: Value, hver: u32, ver: u64, st: &mut LeafState) -> bool {
        let cb = cls.bits();
        if cb == st.stamp_cls && hver == st.stamp_hver && ver == st.stamp_ver {
            return true;
        }
        if self.py_hit_stamp(k, cls, hver, ver) {
            st.stamp_cls = cb;
            st.stamp_hver = hver;
            st.stamp_ver = ver;
            true
        } else {
            false
        }
    }

    /// Check whether `PySetAttr` at `(fid, ip)` can execute against `obj` in
    /// layout mode (`ATTR_SET_L` / `ATTR_APPEND_L`) or via a leaf `@property`
    /// setter (`ATTR_PROP`) without allocating or pushing a frame.
    #[inline]
    fn py_leaf_set_check(&self, fid: u32, ip: usize, obj: Value, allow_prop: bool, st: &mut LeafState) -> Option<LeafSetKind> {
        let k = self.py_hit_keys()?;
        let (cls, dict) = self.py_leaf_parts(k, obj, st)?;
        let e = self.py_ic_for_class(fid, ip, cls.bits());
        if cls.bits() != e.a {
            return None;
        }
        match e.kind {
            ic::ATTR_SET_L => {
                if !self.py_leaf_stamp(k, cls, e.hver, e.c, st) {
                    return None;
                }
                let (l, n) = self.py_leaf_attrs_of(dict, st)?;
                if l == e.d && (e.pos as usize) < n {
                    Some(LeafSetKind::Layout {
                        dict_idx: dict.heap_index(),
                        pos: e.pos,
                        next_layout: l,
                        is_append: false,
                    })
                } else {
                    None
                }
            }
            ic::ATTR_APPEND_L => {
                if !self.py_leaf_stamp(k, cls, e.hver, e.c, st) {
                    return None;
                }
                let (l, n) = self.py_leaf_attrs_of(dict, st)?;
                if l == e.d && (e.pos as usize) == n {
                    Some(LeafSetKind::Layout {
                        dict_idx: dict.heap_index(),
                        pos: e.pos,
                        next_layout: e.b as u32,
                        is_append: true,
                    })
                } else {
                    None
                }
            }
            ic::ATTR_PROP if allow_prop => {
                if !self.py_leaf_stamp(k, cls, e.hver, e.c, st) {
                    return None;
                }
                let ty = self.py_rt.as_deref()?.ty?;
                let acc = self.py_accessor(cls, ty.sp, &e, "c2", 2)?;
                Some(LeafSetKind::PropSetter { fid: acc.fid, fv: acc.f })
            }
            _ => None,
        }
    }

    /// Read `PyGetAttr` at `(fid, ip)` on `obj` in a leaf body without
    /// allocating or pushing a frame: supports `ATTR_GET_L`, `GET_CLASS_L`,
    /// and trivial `@property` getters (`ATTR_PROP` -> `py_prop_inline_plan`).
    #[inline]
    fn py_leaf_attr_get(&self, fid: u32, ip: usize, obj: Value, st: &mut LeafState) -> Option<LeafAttrGet> {
        let k = self.py_hit_keys()?;
        let (cls, dict) = self.py_leaf_parts(k, obj, st)?;
        let e = self.py_ic_for_class(fid, ip, cls.bits());
        if cls.bits() != e.a {
            return None;
        }
        match e.kind {
            ic::ATTR_GET_L => {
                if !self.py_leaf_stamp(k, cls, e.hver, e.c, st) {
                    return None;
                }
                let dict_idx = dict.heap_index();
                // Fast hit when layout is already cached and the slot was just written
                // in this leaf call (e.g. `self.value += d; return self.value`): skips
                // re-reading `HeapObj::PyAttrs` completely.
                if st.n_pending > 0 && dict.bits() == st.last_dict.bits() && st.last_dict_layout == e.d {
                    if (e.pos as u32) < st.last_dict_len {
                        for w in st.pending[..st.n_pending].iter().rev() {
                            if w.dict_idx == dict_idx {
                                if w.pos == e.pos {
                                    return Some(LeafAttrGet::Slot {
                                        dict_idx,
                                        pos: e.pos,
                                        val: w.val,
                                    });
                                }
                                if w.is_append() {
                                    return None;
                                }
                            }
                        }
                    }
                }
                let HeapObj::PyAttrs { layout, vals } = self.heap.get(dict_idx) else {
                    return None;
                };
                if dict.bits() == st.last_dict.bits() {
                    st.last_dict_layout = *layout;
                    st.last_dict_len = vals.len() as u32;
                }
                if *layout != e.d {
                    return None;
                }
                let v = *vals.get(e.pos as usize)?;
                Some(LeafAttrGet::Slot {
                    dict_idx,
                    pos: e.pos,
                    val: v,
                })
            }
            ic::GET_CLASS_L => {
                if self.py_leaf_attrs_of(dict, st).is_some_and(|(l, _)| l == e.d)
                    && self.py_leaf_stamp(k, cls, e.hver, e.c, st)
                {
                    let ty = self.py_rt.as_deref()?.ty?;
                    let v = self.py_table_at(cls, ty.gv, e.b, e.pos)?;
                    (!v.is_undefined() && v != Value::HOLE).then_some(LeafAttrGet::ClassVal {
                        dict_idx: dict.heap_index(),
                        val: v,
                    })
                } else {
                    None
                }
            }
            ic::ATTR_PROP => {
                if !self.py_leaf_stamp(k, cls, e.hver, e.c, st) {
                    return None;
                }
                let (layout, slot) = self.py_prop_inline_plan(cls, false, &e)?;
                let dict_idx = dict.heap_index();
                let HeapObj::PyAttrs { layout: cur_l, vals } = self.heap.get(dict_idx) else {
                    return None;
                };
                if dict.bits() == st.last_dict.bits() {
                    st.last_dict_layout = *cur_l;
                    st.last_dict_len = vals.len() as u32;
                }
                if *cur_l != layout {
                    return None;
                }
                let v = *vals.get(slot as usize)?;
                Some(LeafAttrGet::Slot {
                    dict_idx,
                    pos: slot,
                    val: v,
                })
            }
            _ => None,
        }
    }

    /// Read `PyClassAttr` at `(fid, ip)` on `obj` in a leaf body.
    #[inline]
    fn py_leaf_class_attr_get(&self, fid: u32, ip: usize, obj: Value, st: &mut LeafState) -> Option<LeafAttrGet> {
        let k = self.py_hit_keys()?;
        let (cls, dict) = self.py_leaf_parts(k, obj, st)?;
        let e = self.py_ic_for_class(fid, ip, cls.bits());
        if e.kind != ic::CLASS_ATTR || cls.bits() != e.a || e.d == 0 {
            return None;
        }
        if !self.py_attrs_of(dict).is_some_and(|(l, _)| l == e.d) || !self.py_leaf_stamp(k, cls, e.hver, e.c, st) {
            return None;
        }
        let ty = self.py_rt.as_deref()?.ty?;
        let v = self.py_table_at(cls, ty.gv, e.b, e.pos)?;
        (!v.is_undefined() && v != Value::HOLE).then_some(LeafAttrGet::ClassVal {
            dict_idx: dict.heap_index(),
            val: v,
        })
    }

    /// Read `PyMethod` at `(fid, ip)` on user-class instance `obj` in a leaf body.
    #[inline]
    fn py_leaf_method_get(&self, fid: u32, ip: usize, obj: Value, st: &mut LeafState) -> Option<LeafAttrGet> {
        let k = self.py_hit_keys()?;
        let (cls, dict) = self.py_leaf_parts(k, obj, st)?;
        let e = self.py_ic_for_class(fid, ip, cls.bits());
        if e.kind != ic::METHOD || cls.bits() != e.a || e.d == 0 {
            return None;
        }
        if !self.py_attrs_of(dict).is_some_and(|(l, _)| l == e.d) || !self.py_leaf_stamp(k, cls, e.hver, e.c, st) {
            return None;
        }
        // `ic::METHOD` stores the method record directly in `e.b` (not a table).
        let v = Value::from_bits(e.b);
        (!v.is_undefined() && v != Value::HOLE).then_some(LeafAttrGet::ClassVal {
            dict_idx: dict.heap_index(),
            val: v,
        })
    }

    /// Resolve a nested `PyCall` callee inside a leaf body: tries the site's
    /// `ic::CALL_LEAF` entry first, then falls back to inspecting `cfv.c<argc>`
    /// directly for polymorphic method sites (e.g. `src.withdraw(cents)`).
    #[inline]
    fn py_leaf_resolve_call(&mut self, fid: u32, ip: usize, cfv: Value, argc: u16) -> Option<(u32, LeafInfo)> {
        if !cfv.is_heap() {
            return None;
        }
        let cfi = cfv.heap_index();
        let e = self.py_ic(fid, ip);
        if e.kind == ic::CALL_LEAF && e.c != 0 && e.a == cfv.bits() {
            let (cfid, cparams) = (e.c as u32, (e.c >> 48) as u16);
            if argc <= cparams && self.heap.version_of(cfi) == e.hver {
                let pos = e.pos as usize;
                if matches!(
                    self.heap.get(cfi),
                    HeapObj::Object(m) if pos < m.slot_len() && !m.is_accessor_at(pos) && m.val_at(pos).bits() == e.b
                ) {
                    let cinfo = self.py_leaf_info(cfid)?;
                    return Some((cfid, cinfo));
                }
            }
        }
        const ENTRY_KEYS: [&str; 19] = [
            "c0", "c1", "c2", "c3", "c4", "c5", "c6", "c7", "c8", "c9", "c10", "c11", "c12",
            "c13", "c14", "c15", "c16", "c17", "c18",
        ];
        let key = *ENTRY_KEYS.get(argc as usize)?;
        let entry = self.py_own_data(cfi, key)?;
        if !entry.is_heap() {
            return None;
        }
        let cfid = match self.heap.get(entry.heap_index()) {
            HeapObj::Func(id) => *id,
            HeapObj::Closure { func, .. } => *func,
            _ => return None,
        };
        let cinfo = self.py_leaf_info(cfid)?;
        (argc as u8 <= cinfo.params).then_some((cfid, cinfo))
    }

    /// Direct entry for JIT inline-guarded leaf calls (`jit_py_leaf_noalloc`):
    /// skips `py_leaf_info` lookup and copies raw `u64` argument bits ONCE
    /// directly into the leaf register file (no intermediate `[Value; 12]`).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    #[inline]
    pub(crate) fn py_leaf_exec_bits(&mut self, fid: u32, fv: Value, info: LeafInfo, args: &[u64]) -> Option<Value> {
        let n = args.len();
        if (self.instr_rec.is_some() && !self.native_kernel_admits(LEAF_MAX_STEPS as u64, 0))
            || n > info.params as usize
        {
            return None;
        }
        let mut st = LeafState::new();
        let slots = info.reg_slots as usize;
        let ret = if slots <= LEAF_SMALL_REGS {
            let mut regs = [Value::UNDEFINED; LEAF_SMALL_REGS];
            regs[0] = fv;
            for (dst, &bits) in regs[1..1 + n].iter_mut().zip(args.iter()) {
                *dst = Value::from_bits(bits);
            }
            self.py_leaf_run_inner(fid, fv, info, n, &mut regs, &mut st, 0)?
        } else if slots <= LEAF_MED_REGS {
            let mut regs = [Value::UNDEFINED; LEAF_MED_REGS];
            regs[0] = fv;
            for (dst, &bits) in regs[1..1 + n].iter_mut().zip(args.iter()) {
                *dst = Value::from_bits(bits);
            }
            self.py_leaf_run_inner(fid, fv, info, n, &mut regs, &mut st, 0)?
        } else {
            let mut regs = [Value::UNDEFINED; LEAF_MAX_REGS];
            regs[0] = fv;
            for (dst, &bits) in regs[1..1 + n].iter_mut().zip(args.iter()) {
                *dst = Value::from_bits(bits);
            }
            self.py_leaf_run_inner(fid, fv, info, n, &mut regs, &mut st, 0)?
        };
        self.py_leaf_commit(&st);
        Some(ret)
    }

    /// Fast check whether `fid` is already cached as non-rec (`PyRecSlot::No`).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    #[inline]
    pub(crate) fn py_rec_is_no(&self, fid: u32) -> bool {
        matches!(
            self.py_rt.as_deref().and_then(|p| p.rec_jits.get(fid as usize)),
            Some(crate::codegen::PyRecSlot::No)
        )
    }

    /// Execute the leaf code object `fid` (with function record `fv` and
    /// positional `args`) directly in a stack register file without pushing a
    /// frame or allocating on the VM heap. Returns `None` with zero side
    /// effects whenever anything cannot complete purely here.
    #[inline(never)]
    pub(crate) fn py_leaf_exec(&mut self, fid: u32, fv: Value, args: &[Value]) -> Option<Value> {
        if super::prof_py::on()
            || (self.instr_rec.is_some() && !self.native_kernel_admits(LEAF_MAX_STEPS as u64, 0))
        {
            return None;
        }
        let info = self.py_leaf_info(fid)?;
        let params = info.params as usize;
        let argc = args.len();
        if argc > params {
            return None;
        }
        let mut st = LeafState::new();
        let ret = self.py_leaf_dispatch(fid, fv, info, args, &mut st, 0)?;
        self.py_leaf_commit(&st);
        Some(ret)
    }

    /// Variant of [`Vm::py_leaf_exec`] reading positional arguments directly
    /// from the caller's register window (avoiding a second stack copy).
    #[inline(never)]
    pub(crate) fn py_leaf_exec_window(&mut self, fid: u32, fv: Value, base: usize, arg_base: u16, argc: u16) -> Option<Value> {
        if super::prof_py::on()
            || (self.instr_rec.is_some() && !self.native_kernel_admits(LEAF_MAX_STEPS as u64, 0))
        {
            return None;
        }
        let info = self.py_leaf_info(fid)?;
        let params = info.params as usize;
        let n = argc as usize;
        if n > params {
            return None;
        }
        let mut st = LeafState::new();
        let start = base + arg_base as usize;
        let slots = info.reg_slots as usize;
        let ret = if slots <= LEAF_SMALL_REGS {
            let mut regs = [Value::UNDEFINED; LEAF_SMALL_REGS];
            regs[0] = fv;
            regs[1..1 + n].copy_from_slice(&self.regs[start..start + n]);
            self.py_leaf_run_inner(fid, fv, info, n, &mut regs, &mut st, 0)?
        } else if slots <= LEAF_MED_REGS {
            let mut regs = [Value::UNDEFINED; LEAF_MED_REGS];
            regs[0] = fv;
            regs[1..1 + n].copy_from_slice(&self.regs[start..start + n]);
            self.py_leaf_run_inner(fid, fv, info, n, &mut regs, &mut st, 0)?
        } else {
            let mut regs = [Value::UNDEFINED; LEAF_MAX_REGS];
            regs[0] = fv;
            regs[1..1 + n].copy_from_slice(&self.regs[start..start + n]);
            self.py_leaf_run_inner(fid, fv, info, n, &mut regs, &mut st, 0)?
        };
        self.py_leaf_commit(&st);
        Some(ret)
    }

    #[inline(always)]
    fn py_leaf_commit(&mut self, st: &LeafState) {
        if let Some((rt_idx, slot)) = st.pending_mself {
            if let HeapObj::Object(m) = self.heap.get_mut(rt_idx) {
                m.set_val_at(slot, Value::TRUE);
            }
        }
        for w in &st.pending[..st.n_pending] {
            if w.is_append() {
                self.py_attrs_push(w.dict_idx, w.next_layout(), w.val);
            } else {
                self.py_attrs_put(w.dict_idx, w.pos as usize, w.val);
            }
        }
        if self.instr_rec.is_some() {
            self.charge_steps(st.steps);
        }
    }

    #[inline(always)]
    fn py_leaf_dispatch(
        &mut self,
        fid: u32,
        fv: Value,
        info: LeafInfo,
        args: &[Value],
        st: &mut LeafState,
        depth: u8,
    ) -> Option<Value> {
        let n = args.len();
        let slots = info.reg_slots as usize;
        if slots <= LEAF_SMALL_REGS {
            let mut regs = [Value::UNDEFINED; LEAF_SMALL_REGS];
            regs[0] = fv;
            regs[1..1 + n].copy_from_slice(args);
            self.py_leaf_run_inner(fid, fv, info, n, &mut regs, st, depth)
        } else if slots <= LEAF_MED_REGS {
            let mut regs = [Value::UNDEFINED; LEAF_MED_REGS];
            regs[0] = fv;
            regs[1..1 + n].copy_from_slice(args);
            self.py_leaf_run_inner(fid, fv, info, n, &mut regs, st, depth)
        } else {
            let mut regs = [Value::UNDEFINED; LEAF_MAX_REGS];
            regs[0] = fv;
            regs[1..1 + n].copy_from_slice(args);
            self.py_leaf_run_inner(fid, fv, info, n, &mut regs, st, depth)
        }
    }

    #[inline(always)]
    fn py_leaf_get_prop(&self, ov: Value, key: &str, pending_mself: bool) -> Option<Value> {
        let idx = ov.heap_index();
        if let Some(p) = self.py_rt.as_deref() {
            if ov.bits() == p.r.bits() {
                let fast = match key {
                    "SMFNATIVE" => p.smfnative,
                    "TLIST" => p.t_list,
                    "TTUPLE" => p.t_tuple,
                    "TDICT" => p.t_dict,
                    "mself" => {
                        if pending_mself {
                            return Some(Value::TRUE);
                        }
                        if let Some(slot) = p.mself_slot {
                            if let HeapObj::Object(m) = self.heap.get(idx) {
                                if slot < m.slot_len() && !m.is_accessor_at(slot) {
                                    return Some(m.val_at(slot));
                                }
                            }
                        }
                        Value::UNDEFINED
                    }
                    _ => Value::UNDEFINED,
                };
                if !fast.is_undefined() {
                    return Some(fast);
                }
            }
        }
        if key == "v" {
            if let HeapObj::Object(m) = self.heap.get(idx) {
                if !m.is_ctor && m.slot_len() > 0 && m.key_at(0) == "v" && !m.is_accessor_at(0) {
                    return Some(m.val_at(0));
                }
            }
        } else if key == "cells" {
            return self.py_hint_field(super::super::py_rt::hint::CELLS, idx, "cells");
        }
        self.py_own_data(idx, key)
    }

    fn py_leaf_run_inner(
        &mut self,
        fid: u32,
        fv: Value,
        info: LeafInfo,
        argc: usize,
        regs: &mut [Value],
        st: &mut LeafState,
        depth: u8,
    ) -> Option<Value> {
        let params = info.params as usize;
        // Fill missing positional parameters from `fv.defaults` (same check as
        // `Vm::py_fill_defaults`), so the prologue's default check skips cleanly.
        if argc < params {
            if !fv.is_heap() {
                return None;
            }
            let d = self.py_hint_field(super::super::py_rt::hint::DEFAULTS, fv.heap_index(), "defaults")?;
            if !d.is_heap() || (!self.array_js_len.is_empty() && self.array_js_len.contains_key(&d.heap_index())) {
                return None;
            }
            let HeapObj::Array(items) = self.heap.get(d.heap_index()) else {
                return None;
            };
            let first = params.checked_sub(items.len())?;
            if argc < first {
                return None;
            }
            for i in argc..params {
                let v = items[i - first];
                if v == Value::HOLE || v.is_undefined() {
                    return None;
                }
                regs[1 + i] = v;
            }
        }

        let main_end = info.main_end as usize;
        let main_proto: Option<&'p crate::bytecode::FuncProto> =
            ((fid as usize) < self.main_func_count).then(|| &self.program.functions[fid as usize]);
        let main_code: Option<&'p [Instr]> = main_proto.map(|p| &p.code[..main_end]);
        let mut ip = 0usize;
        let mut handler_depth: u32 = 0;

        while ip < main_end {
            st.steps += 1;
            if st.steps > LEAF_MAX_STEPS {
                return None;
            }
            let instr = match main_code {
                Some(c) => c[ip],
                None => self.eval_funcs[fid as usize - self.main_func_count].code[ip],
            };
            match instr {
                Instr::LoadGlobal { dst, idx } => {
                    let v = *self.globals.get(idx as usize)?;
                    if v == Value::UNINITIALIZED {
                        return None;
                    }
                    regs[dst as usize] = v;
                    ip += 1;
                }
                Instr::Move { dst, src } => {
                    regs[dst as usize] = regs[src as usize];
                    ip += 1;
                }
                Instr::LoadNull { dst } => {
                    regs[dst as usize] = Value::NULL;
                    ip += 1;
                }
                Instr::LoadUndefined { dst } => {
                    regs[dst as usize] = Value::UNDEFINED;
                    ip += 1;
                }
                Instr::LoadBool { dst, val } => {
                    regs[dst as usize] = Value::bool(val);
                    ip += 1;
                }
                Instr::LoadInt { dst, val } => {
                    regs[dst as usize] = Value::int(val);
                    ip += 1;
                }
                Instr::LoadBigInt { dst, value } => {
                    if let Some(sb) = Value::small_bigint(value.get()) {
                        regs[dst as usize] = sb;
                        ip += 1;
                    } else if (dst as usize) < 16 {
                        st.steps -= 1;
                        return self.py_leaf_run_wide(fid, ip, main_end, handler_depth, regs, st, depth);
                    } else {
                        return None;
                    }
                }
                Instr::LoadConst { dst, idx } => {
                    let raw = *match main_proto {
                        Some(p) => p.constants.get(idx as usize),
                        None => self.eval_funcs[fid as usize - self.main_func_count].constants.get(idx as usize),
                    }?;
                    let v = if raw.is_heap() && (raw.heap_index() & crate::vm::helpers_misc::STRING_CONST_BIT) != 0 {
                        let cached = self
                            .const_string_dense
                            .get(fid as usize)
                            .and_then(|f| f.get(idx as usize))
                            .copied()?;
                        if cached.is_undefined() {
                            return None;
                        }
                        cached
                    } else {
                        raw
                    };
                    regs[dst as usize] = v;
                    ip += 1;
                }
                Instr::GetProp { dst, obj, name } => {
                    let ov = regs[obj as usize];
                    if !ov.is_heap() {
                        return None;
                    }
                    let key = match main_proto {
                        Some(p) => p.string_constants.get(name as usize),
                        None => self.eval_funcs[fid as usize - self.main_func_count]
                            .string_constants
                            .get(name as usize),
                    }?
                    .as_str();
                    let v = self.py_leaf_get_prop(ov, key, st.pending_mself.is_some())?;
                    regs[dst as usize] = v;
                    ip += 1;
                }
                Instr::GetIndex { dst, obj, key } => {
                    let (ov, kv) = (regs[obj as usize], regs[key as usize]);
                    if !ov.is_heap() || !kv.is_int() {
                        return None;
                    }
                    let i = kv.as_int();
                    if i < 0 {
                        return None;
                    }
                    let oi = ov.heap_index();
                    if !self.array_js_len.is_empty() && self.array_js_len.contains_key(&oi) {
                        return None;
                    }
                    let HeapObj::Array(items) = self.heap.get(oi) else {
                        return None;
                    };
                    let v = *items.get(i as usize)?;
                    if v == Value::HOLE {
                        return None;
                    }
                    regs[dst as usize] = v;
                    ip += 1;
                }
                Instr::TypeOfIs { dst, a, code, neg } => {
                    let eq = if let Some(&want) = crate::bytecode::TYPEOF_NAMES.get(code as usize) {
                        let v = regs[a as usize];
                        let got = if v.is_undefined() {
                            "undefined"
                        } else if v.is_small_bigint() {
                            "bigint"
                        } else if v.is_number() {
                            "number"
                        } else if v == Value::TRUE || v == Value::FALSE {
                            "boolean"
                        } else {
                            self.type_of(v)
                        };
                        want == got
                    } else {
                        false
                    };
                    regs[dst as usize] = Value::bool(eq != neg);
                    ip += 1;
                }
                Instr::Not { dst, a } => {
                    let v = regs[a as usize];
                    if v == Value::TRUE {
                        regs[dst as usize] = Value::FALSE;
                    } else if v == Value::FALSE {
                        regs[dst as usize] = Value::TRUE;
                    } else {
                        return None;
                    }
                    ip += 1;
                }
                Instr::Neg { dst, a } => {
                    let v = regs[a as usize];
                    if let Some(n) = v.small_bigint_val() {
                        if let Some(sb) = (n as i128).checked_neg().and_then(Value::small_bigint) {
                            regs[dst as usize] = sb;
                            ip += 1;
                        } else if (dst as usize) < 16 {
                            st.steps -= 1;
                            return self.py_leaf_run_wide(fid, ip, main_end, handler_depth, regs, st, depth);
                        } else {
                            return None;
                        }
                    } else if v.is_number() {
                        regs[dst as usize] = Value::num(-v.as_f64());
                        ip += 1;
                    } else {
                        return None;
                    }
                }
                Instr::BitNot { dst, a } => {
                    let n = regs[a as usize].small_bigint_val()?;
                    if let Some(sb) = Value::small_bigint(!(n as i128)) {
                        regs[dst as usize] = sb;
                        ip += 1;
                    } else if (dst as usize) < 16 {
                        st.steps -= 1;
                        return self.py_leaf_run_wide(fid, ip, main_end, handler_depth, regs, st, depth);
                    } else {
                        return None;
                    }
                }
                Instr::AddInt { dst, a, imm, upd } => {
                    let va = regs[a as usize];
                    if va.is_int() {
                        regs[dst as usize] = match va.as_int().checked_add(imm) {
                            Some(v) => Value::int(v),
                            None => Value::num(va.as_int() as f64 + imm as f64),
                        };
                    } else if va.is_double() {
                        regs[dst as usize] = Value::num(va.as_f64() + imm as f64);
                    } else if upd {
                        let x = va.small_bigint_val()?;
                        if let Some(sb) = Value::small_bigint((x as i128) + (imm as i128)) {
                            regs[dst as usize] = sb;
                        } else if (dst as usize) < 16 {
                            st.steps -= 1;
                            return self.py_leaf_run_wide(fid, ip, main_end, handler_depth, regs, st, depth);
                        } else {
                            return None;
                        }
                    } else {
                        return None;
                    }
                    ip += 1;
                }
                Instr::Eq { dst, a, b } | Instr::Ne { dst, a, b } => {
                    let (va, vb) = (regs[a as usize], regs[b as usize]);
                    let eq = if va.bits() == vb.bits() {
                        !(va.is_double() && va.as_f64().is_nan())
                    } else if va.is_number() && vb.is_number() {
                        va.as_f64() == vb.as_f64()
                    } else if (va == Value::NULL || va == Value::TRUE || va == Value::FALSE || va.is_small_bigint())
                        && (vb == Value::NULL || vb == Value::TRUE || vb == Value::FALSE || vb.is_small_bigint())
                    {
                        false
                    } else {
                        return None;
                    };
                    let is_ne = matches!(instr, Instr::Ne { .. });
                    regs[dst as usize] = Value::bool(eq != is_ne);
                    ip += 1;
                }
                Instr::PyAddImm { dst, a, imm, .. } => {
                    let va = regs[a as usize];
                    if let Some(x) = va.small_bigint_val() {
                        if let Some(sb) = Value::small_bigint((x as i128) + (imm as i128)) {
                            regs[dst as usize] = sb;
                        } else if (dst as usize) < 16 {
                            st.steps -= 1;
                            return self.py_leaf_run_wide(fid, ip, main_end, handler_depth, regs, st, depth);
                        } else {
                            return None;
                        }
                    } else if va.is_number() {
                        regs[dst as usize] = Value::num(va.as_f64() + imm as f64);
                    } else {
                        return None;
                    }
                    ip += 1;
                }
                Instr::PyArith { op, dst, a, b, .. } => {
                    let (va, vb) = (regs[a as usize], regs[b as usize]);
                    if let (Some(x), Some(y)) = (va.small_bigint_val(), vb.small_bigint_val()) {
                        if let Some(sb) = py_small_arith(op, x, y).and_then(|r| Value::small_bigint(r as i128)) {
                            regs[dst as usize] = sb;
                        } else if (dst as usize) < 16 {
                            st.steps -= 1;
                            return self.py_leaf_run_wide(fid, ip, main_end, handler_depth, regs, st, depth);
                        } else {
                            return None;
                        }
                    } else if va.is_number() && vb.is_number() {
                        regs[dst as usize] = leaf_arith_num(op, va, vb)?;
                    } else {
                        return None;
                    }
                    ip += 1;
                }
                Instr::PyCompare { op, dst, a, b, .. } => {
                    let r = self.py_compare(op, regs[a as usize], regs[b as usize]).ok()??;
                    regs[dst as usize] = Value::bool(r);
                    ip += 1;
                }
                Instr::PyJumpCompare { op, a, b, when, target, .. } => {
                    let r = self.py_compare(op, regs[a as usize], regs[b as usize]).ok()??;
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
                        return None;
                    }
                }
                Instr::JumpIfTrue { cond, target } => {
                    let v = regs[cond as usize];
                    if v == Value::TRUE {
                        ip = target as usize;
                    } else if v == Value::FALSE {
                        ip += 1;
                    } else {
                        return None;
                    }
                }
                Instr::JumpIfNotLt { a, b, target } => {
                    let (va, vb) = (regs[a as usize], regs[b as usize]);
                    let lt = if let (Some(x), Some(y)) = (va.small_bigint_val(), vb.small_bigint_val()) {
                        x < y
                    } else if va.is_number() && vb.is_number() {
                        va.as_f64() < vb.as_f64()
                    } else {
                        return None;
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
                        return None;
                    };
                    if !le {
                        ip = target as usize;
                    } else {
                        ip += 1;
                    }
                }
                Instr::PyGetAttr { dst, obj, .. } => {
                    let ov = regs[obj as usize];
                    let v = match self.py_leaf_attr_get(fid, ip, ov, st)? {
                        LeafAttrGet::Slot { dict_idx, pos, val } => {
                            let mut out = val;
                            if st.n_pending > 0 {
                                for w in st.pending[..st.n_pending].iter().rev() {
                                    if w.dict_idx == dict_idx {
                                        if w.pos == pos {
                                            out = w.val;
                                            break;
                                        }
                                        if w.is_append() {
                                            return None;
                                        }
                                    }
                                }
                            }
                            out
                        }
                        LeafAttrGet::ClassVal { dict_idx, val } => {
                            if st.n_pending > 0
                                && st.pending[..st.n_pending]
                                    .iter()
                                    .any(|w| w.dict_idx == dict_idx && w.is_append())
                            {
                                return None;
                            }
                            val
                        }
                    };
                    regs[dst as usize] = v;
                    ip += 1;
                }
                Instr::PyClassAttr { dst, obj, .. } => {
                    let ov = regs[obj as usize];
                    let LeafAttrGet::ClassVal { dict_idx, val } = self.py_leaf_class_attr_get(fid, ip, ov, st)? else {
                        return None;
                    };
                    if st.n_pending > 0
                        && st.pending[..st.n_pending]
                            .iter()
                            .any(|w| w.dict_idx == dict_idx && w.is_append())
                    {
                        return None;
                    }
                    regs[dst as usize] = val;
                    ip += 1;
                }
                Instr::PySetAttr { obj, val, .. } => {
                    let ov = regs[obj as usize];
                    let vv = regs[val as usize];
                    match self.py_leaf_set_check(fid, ip, ov, depth + 1 < LEAF_MAX_DEPTH, st)? {
                        LeafSetKind::Layout { dict_idx, pos, next_layout, is_append } => {
                            if st.n_pending >= LEAF_MAX_WRITES {
                                return None;
                            }
                            if st.n_pending > 0
                                && st.pending[..st.n_pending]
                                    .iter()
                                    .any(|w| w.dict_idx == dict_idx && (w.is_append() || is_append))
                            {
                                return None;
                            }
                            st.pending[st.n_pending] = PendingAttr {
                                dict_idx,
                                pos,
                                layout_flags: next_layout | if is_append { PENDING_APPEND_BIT } else { 0 },
                                val: vv,
                            };
                            st.n_pending += 1;
                        }
                        LeafSetKind::PropSetter { fid: sfid, fv: sfv } => {
                            let sinfo = self.py_leaf_info(sfid)?;
                            if (sinfo.params as usize) < 2 {
                                return None;
                            }
                            self.py_leaf_dispatch(sfid, sfv, sinfo, &[ov, vv], st, depth + 1)?;
                        }
                    }
                    ip += 1;
                }
                Instr::CallWithThis { dst, callee, this_v, arg_base, argc, .. } => {
                    if argc != 3 || !self.realm_global_objs.is_empty() || !self.realm_fns.is_empty() {
                        return None;
                    }
                    let cv = regs[callee as usize];
                    if !cv.is_heap()
                        || !matches!(self.heap.get(cv.heap_index()), HeapObj::Native(id) if *id == super::super::native::PY_SMFIND)
                    {
                        return None;
                    }
                    let ab = arg_base as usize;
                    let rt_v = regs[this_v as usize];
                    let (found, write_slot) = self.py_smfind_hit(regs[ab], regs[ab + 1], regs[ab + 2], rt_v)?;
                    if let Some(slot) = write_slot {
                        st.pending_mself = Some((rt_v.heap_index(), slot));
                    }
                    regs[dst as usize] = found;
                    ip += 1;
                }
                Instr::PyMethod { dst, obj, .. } => {
                    let ov = regs[obj as usize];
                    let LeafAttrGet::ClassVal { dict_idx, val } = self.py_leaf_method_get(fid, ip, ov, st)? else {
                        return None;
                    };
                    if st.n_pending > 0
                        && st.pending[..st.n_pending]
                            .iter()
                            .any(|w| w.dict_idx == dict_idx && w.is_append())
                    {
                        return None;
                    }
                    regs[dst as usize] = val;
                    ip += 1;
                }
                Instr::PushHandler { .. } => {
                    handler_depth += 1;
                    ip += 1;
                }
                Instr::PopHandler => {
                    if handler_depth == 0 {
                        return None;
                    }
                    handler_depth -= 1;
                    ip += 1;
                }
                Instr::PyCall { dst, f, arg_base, argc, .. } => {
                    if depth + 1 >= LEAF_MAX_DEPTH || (argc as usize) > LEAF_MAX_PARAMS {
                        return None;
                    }
                    let cfv = regs[f as usize];
                    let (cfid, cinfo) = self.py_leaf_resolve_call(fid, ip, cfv, argc)?;
                    let n = argc as usize;
                    let ab = arg_base as usize;
                    let res = self.py_leaf_dispatch(cfid, cfv, cinfo, &regs[ab..ab + n], st, depth + 1)?;
                    regs[dst as usize] = res;
                    ip += 1;
                }
                Instr::PyGetItem { dst, o, k, seq, dict, .. } => {
                    let Some(Some(v)) = self
                        .py_get_item(regs[o as usize], regs[k as usize], regs[seq as usize], regs[dict as usize])
                        .ok()?
                    else {
                        return None;
                    };
                    regs[dst as usize] = v;
                    ip += 1;
                }
                Instr::PyLen { dst, v, rt, .. } => {
                    let n = self.py_len(regs[v as usize], regs[rt as usize])?;
                    regs[dst as usize] = Value::small_bigint(n as i128)?;
                    ip += 1;
                }
                Instr::PyTruth { dst, v, rt, .. } => {
                    let b = self.py_truth(regs[v as usize], regs[rt as usize])?;
                    regs[dst as usize] = Value::bool(b);
                    ip += 1;
                }
                Instr::Return { src } => {
                    if handler_depth != 0 {
                        return None;
                    }
                    return Some(regs[src as usize]);
                }
                _ => return None,
            }
        }
        None
    }

    /// Cold continuation of [`Vm::py_leaf_run_inner`] with an unboxed `i128`
    /// side-band for intermediate integers exceeding 47-bit small BigInt range
    /// (e.g. LCG `state * 1103515245 + 12345` before `% 2^31`). Never on the
    /// hot stack frame for normal small-int/float leaf calls.
    #[cold]
    #[inline(never)]
    #[allow(clippy::too_many_arguments)]
    fn py_leaf_run_wide(
        &mut self,
        fid: u32,
        mut ip: usize,
        main_end: usize,
        mut handler_depth: u32,
        regs: &mut [Value],
        st: &mut LeafState,
        depth: u8,
    ) -> Option<Value> {
        const WIDE_REGS: usize = 16;
        let mut wide_mask: u16 = 0;
        let mut wide = [0i128; WIDE_REGS];

        #[inline(always)]
        fn is_wide(mask: u16, r: u16) -> bool {
            r < WIDE_REGS as u16 && (mask & (1u16 << r)) != 0
        }
        #[inline(always)]
        fn clear_wide(mask: &mut u16, r: u16) {
            if *mask != 0 && r < WIDE_REGS as u16 {
                *mask &= !(1u16 << r);
            }
        }
        #[inline(always)]
        fn read_int128(mask: u16, wide: &[i128; WIDE_REGS], regs: &[Value], r: u16) -> Option<i128> {
            if is_wide(mask, r) {
                Some(wide[r as usize])
            } else {
                regs[r as usize].small_bigint_val().map(|x| x as i128)
            }
        }

        while ip < main_end {
            st.steps += 1;
            if st.steps > LEAF_MAX_STEPS {
                return None;
            }
            let instr = self.func(fid as usize).code[ip];
            match instr {
                Instr::LoadGlobal { dst, idx } => {
                    let v = *self.globals.get(idx as usize)?;
                    if v == Value::UNINITIALIZED {
                        return None;
                    }
                    regs[dst as usize] = v;
                    clear_wide(&mut wide_mask, dst);
                    ip += 1;
                }
                Instr::Move { dst, src } => {
                    if is_wide(wide_mask, src) {
                        if (dst as usize) >= WIDE_REGS {
                            return None;
                        }
                        wide[dst as usize] = wide[src as usize];
                        wide_mask |= 1u16 << dst;
                    } else {
                        regs[dst as usize] = regs[src as usize];
                        clear_wide(&mut wide_mask, dst);
                    }
                    ip += 1;
                }
                Instr::LoadNull { dst } => {
                    regs[dst as usize] = Value::NULL;
                    clear_wide(&mut wide_mask, dst);
                    ip += 1;
                }
                Instr::LoadUndefined { dst } => {
                    regs[dst as usize] = Value::UNDEFINED;
                    clear_wide(&mut wide_mask, dst);
                    ip += 1;
                }
                Instr::LoadBool { dst, val } => {
                    regs[dst as usize] = Value::bool(val);
                    clear_wide(&mut wide_mask, dst);
                    ip += 1;
                }
                Instr::LoadInt { dst, val } => {
                    regs[dst as usize] = Value::int(val);
                    clear_wide(&mut wide_mask, dst);
                    ip += 1;
                }
                Instr::LoadBigInt { dst, value } => {
                    let v = value.get();
                    if let Some(sb) = Value::small_bigint(v) {
                        regs[dst as usize] = sb;
                        clear_wide(&mut wide_mask, dst);
                    } else if (dst as usize) < WIDE_REGS {
                        wide[dst as usize] = v;
                        wide_mask |= 1u16 << dst;
                    } else {
                        return None;
                    }
                    ip += 1;
                }
                Instr::LoadConst { dst, idx } => {
                    let raw = *self.func(fid as usize).constants.get(idx as usize)?;
                    let v = if raw.is_heap() && (raw.heap_index() & crate::vm::helpers_misc::STRING_CONST_BIT) != 0 {
                        let cached = self
                            .const_string_dense
                            .get(fid as usize)
                            .and_then(|f| f.get(idx as usize))
                            .copied()?;
                        if cached.is_undefined() {
                            return None;
                        }
                        cached
                    } else {
                        raw
                    };
                    regs[dst as usize] = v;
                    clear_wide(&mut wide_mask, dst);
                    ip += 1;
                }
                Instr::GetProp { dst, obj, name } => {
                    if is_wide(wide_mask, obj) {
                        return None;
                    }
                    let ov = regs[obj as usize];
                    if !ov.is_heap() {
                        return None;
                    }
                    let key = self.func(fid as usize).string_constants.get(name as usize)?.as_str();
                    let v = self.py_leaf_get_prop(ov, key, st.pending_mself.is_some())?;
                    regs[dst as usize] = v;
                    clear_wide(&mut wide_mask, dst);
                    ip += 1;
                }
                Instr::GetIndex { dst, obj, key } => {
                    if is_wide(wide_mask, obj) || is_wide(wide_mask, key) {
                        return None;
                    }
                    let (ov, kv) = (regs[obj as usize], regs[key as usize]);
                    if !ov.is_heap() || !kv.is_int() {
                        return None;
                    }
                    let i = kv.as_int();
                    if i < 0 {
                        return None;
                    }
                    let oi = ov.heap_index();
                    if !self.array_js_len.is_empty() && self.array_js_len.contains_key(&oi) {
                        return None;
                    }
                    let HeapObj::Array(items) = self.heap.get(oi) else {
                        return None;
                    };
                    let v = *items.get(i as usize)?;
                    if v == Value::HOLE {
                        return None;
                    }
                    regs[dst as usize] = v;
                    clear_wide(&mut wide_mask, dst);
                    ip += 1;
                }
                Instr::TypeOfIs { dst, a, code, neg } => {
                    let eq = if let Some(&want) = crate::bytecode::TYPEOF_NAMES.get(code as usize) {
                        let got = if is_wide(wide_mask, a) {
                            "bigint"
                        } else {
                            let v = regs[a as usize];
                            if v.is_undefined() {
                                "undefined"
                            } else if v.is_small_bigint() {
                                "bigint"
                            } else if v.is_number() {
                                "number"
                            } else if v == Value::TRUE || v == Value::FALSE {
                                "boolean"
                            } else {
                                self.type_of(v)
                            }
                        };
                        want == got
                    } else {
                        false
                    };
                    regs[dst as usize] = Value::bool(eq != neg);
                    clear_wide(&mut wide_mask, dst);
                    ip += 1;
                }
                Instr::Not { dst, a } => {
                    if is_wide(wide_mask, a) {
                        return None;
                    }
                    let v = regs[a as usize];
                    if v == Value::TRUE {
                        regs[dst as usize] = Value::FALSE;
                    } else if v == Value::FALSE {
                        regs[dst as usize] = Value::TRUE;
                    } else {
                        return None;
                    }
                    clear_wide(&mut wide_mask, dst);
                    ip += 1;
                }
                Instr::Neg { dst, a } => {
                    if let Some(n) = read_int128(wide_mask, &wide, regs, a) {
                        let r = n.checked_neg()?;
                        if let Some(sb) = Value::small_bigint(r) {
                            regs[dst as usize] = sb;
                            clear_wide(&mut wide_mask, dst);
                        } else if (dst as usize) < WIDE_REGS {
                            wide[dst as usize] = r;
                            wide_mask |= 1u16 << dst;
                        } else {
                            return None;
                        }
                    } else {
                        let v = regs[a as usize];
                        if v.is_number() {
                            regs[dst as usize] = Value::num(-v.as_f64());
                            clear_wide(&mut wide_mask, dst);
                        } else {
                            return None;
                        }
                    }
                    ip += 1;
                }
                Instr::BitNot { dst, a } => {
                    let n = read_int128(wide_mask, &wide, regs, a)?;
                    let r = !n;
                    if let Some(sb) = Value::small_bigint(r) {
                        regs[dst as usize] = sb;
                        clear_wide(&mut wide_mask, dst);
                    } else if (dst as usize) < WIDE_REGS {
                        wide[dst as usize] = r;
                        wide_mask |= 1u16 << dst;
                    } else {
                        return None;
                    }
                    ip += 1;
                }
                Instr::AddInt { dst, a, imm, upd } => {
                    if is_wide(wide_mask, a) {
                        if !upd {
                            return None;
                        }
                        let r = wide[a as usize].checked_add(imm as i128)?;
                        if let Some(sb) = Value::small_bigint(r) {
                            regs[dst as usize] = sb;
                            clear_wide(&mut wide_mask, dst);
                        } else if (dst as usize) < WIDE_REGS {
                            wide[dst as usize] = r;
                            wide_mask |= 1u16 << dst;
                        } else {
                            return None;
                        }
                    } else {
                        let va = regs[a as usize];
                        regs[dst as usize] = if va.is_int() {
                            match va.as_int().checked_add(imm) {
                                Some(v) => Value::int(v),
                                None => Value::num(va.as_int() as f64 + imm as f64),
                            }
                        } else if va.is_double() {
                            Value::num(va.as_f64() + imm as f64)
                        } else if upd {
                            let x = va.small_bigint_val()?;
                            let r = (x as i128) + (imm as i128);
                            if let Some(sb) = Value::small_bigint(r) {
                                sb
                            } else if (dst as usize) < WIDE_REGS {
                                wide[dst as usize] = r;
                                wide_mask |= 1u16 << dst;
                                ip += 1;
                                continue;
                            } else {
                                return None;
                            }
                        } else {
                            return None;
                        };
                        clear_wide(&mut wide_mask, dst);
                    }
                    ip += 1;
                }
                Instr::Eq { dst, a, b } | Instr::Ne { dst, a, b } => {
                    if is_wide(wide_mask, a) || is_wide(wide_mask, b) {
                        return None;
                    }
                    let (va, vb) = (regs[a as usize], regs[b as usize]);
                    let eq = if va.bits() == vb.bits() {
                        !(va.is_double() && va.as_f64().is_nan())
                    } else if va.is_number() && vb.is_number() {
                        va.as_f64() == vb.as_f64()
                    } else if (va == Value::NULL || va == Value::TRUE || va == Value::FALSE || va.is_small_bigint())
                        && (vb == Value::NULL || vb == Value::TRUE || vb == Value::FALSE || vb.is_small_bigint())
                    {
                        false
                    } else {
                        return None;
                    };
                    let is_ne = matches!(instr, Instr::Ne { .. });
                    regs[dst as usize] = Value::bool(eq != is_ne);
                    clear_wide(&mut wide_mask, dst);
                    ip += 1;
                }
                Instr::PyAddImm { dst, a, imm, .. } => {
                    if let Some(x) = read_int128(wide_mask, &wide, regs, a) {
                        let r = x.checked_add(imm as i128)?;
                        if let Some(sb) = Value::small_bigint(r) {
                            regs[dst as usize] = sb;
                            clear_wide(&mut wide_mask, dst);
                        } else if (dst as usize) < WIDE_REGS {
                            wide[dst as usize] = r;
                            wide_mask |= 1u16 << dst;
                        } else {
                            return None;
                        }
                    } else {
                        let va = regs[a as usize];
                        if va.is_number() {
                            regs[dst as usize] = Value::num(va.as_f64() + imm as f64);
                            clear_wide(&mut wide_mask, dst);
                        } else {
                            return None;
                        }
                    }
                    ip += 1;
                }
                Instr::PyArith { op, dst, a, b, .. } => {
                    if let (Some(x), Some(y)) = (
                        read_int128(wide_mask, &wide, regs, a),
                        read_int128(wide_mask, &wide, regs, b),
                    ) {
                        let r = py_i128_arith(op, x, y)?;
                        if let Some(sb) = Value::small_bigint(r) {
                            regs[dst as usize] = sb;
                            clear_wide(&mut wide_mask, dst);
                        } else if (dst as usize) < WIDE_REGS {
                            wide[dst as usize] = r;
                            wide_mask |= 1u16 << dst;
                        } else {
                            return None;
                        }
                    } else if !is_wide(wide_mask, a) && !is_wide(wide_mask, b) {
                        let (va, vb) = (regs[a as usize], regs[b as usize]);
                        if va.is_number() && vb.is_number() {
                            regs[dst as usize] = leaf_arith_num(op, va, vb)?;
                            clear_wide(&mut wide_mask, dst);
                        } else {
                            return None;
                        }
                    } else {
                        return None;
                    }
                    ip += 1;
                }
                Instr::PyCompare { op, dst, a, b, .. } => {
                    let r = if wide_mask != 0 && (is_wide(wide_mask, a) || is_wide(wide_mask, b)) {
                        let x = read_int128(wide_mask, &wide, regs, a)?;
                        let y = read_int128(wide_mask, &wide, regs, b)?;
                        use crate::bytecode::PyCmpOp as C;
                        match op {
                            C::Lt => x < y,
                            C::Le => x <= y,
                            C::Gt => x > y,
                            C::Ge => x >= y,
                            C::Eq => x == y,
                            C::Ne => x != y,
                        }
                    } else {
                        self.py_compare(op, regs[a as usize], regs[b as usize]).ok()??
                    };
                    regs[dst as usize] = Value::bool(r);
                    clear_wide(&mut wide_mask, dst);
                    ip += 1;
                }
                Instr::PyJumpCompare { op, a, b, when, target, .. } => {
                    let r = if wide_mask != 0 && (is_wide(wide_mask, a) || is_wide(wide_mask, b)) {
                        let x = read_int128(wide_mask, &wide, regs, a)?;
                        let y = read_int128(wide_mask, &wide, regs, b)?;
                        use crate::bytecode::PyCmpOp as C;
                        match op {
                            C::Lt => x < y,
                            C::Le => x <= y,
                            C::Gt => x > y,
                            C::Ge => x >= y,
                            C::Eq => x == y,
                            C::Ne => x != y,
                        }
                    } else {
                        self.py_compare(op, regs[a as usize], regs[b as usize]).ok()??
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
                    if is_wide(wide_mask, cond) {
                        return None;
                    }
                    let v = regs[cond as usize];
                    if v == Value::FALSE {
                        ip = target as usize;
                    } else if v == Value::TRUE {
                        ip += 1;
                    } else {
                        return None;
                    }
                }
                Instr::JumpIfTrue { cond, target } => {
                    if is_wide(wide_mask, cond) {
                        return None;
                    }
                    let v = regs[cond as usize];
                    if v == Value::TRUE {
                        ip = target as usize;
                    } else if v == Value::FALSE {
                        ip += 1;
                    } else {
                        return None;
                    }
                }
                Instr::JumpIfNotLt { a, b, target } => {
                    let lt = if let (Some(x), Some(y)) = (
                        read_int128(wide_mask, &wide, regs, a),
                        read_int128(wide_mask, &wide, regs, b),
                    ) {
                        x < y
                    } else if !is_wide(wide_mask, a) && !is_wide(wide_mask, b) {
                        let (va, vb) = (regs[a as usize], regs[b as usize]);
                        if va.is_number() && vb.is_number() {
                            va.as_f64() < vb.as_f64()
                        } else {
                            return None;
                        }
                    } else {
                        return None;
                    };
                    if !lt {
                        ip = target as usize;
                    } else {
                        ip += 1;
                    }
                }
                Instr::JumpIfNotLe { a, b, target } => {
                    let le = if let (Some(x), Some(y)) = (
                        read_int128(wide_mask, &wide, regs, a),
                        read_int128(wide_mask, &wide, regs, b),
                    ) {
                        x <= y
                    } else if !is_wide(wide_mask, a) && !is_wide(wide_mask, b) {
                        let (va, vb) = (regs[a as usize], regs[b as usize]);
                        if va.is_number() && vb.is_number() {
                            va.as_f64() <= vb.as_f64()
                        } else {
                            return None;
                        }
                    } else {
                        return None;
                    };
                    if !le {
                        ip = target as usize;
                    } else {
                        ip += 1;
                    }
                }
                Instr::PyGetAttr { dst, obj, .. } => {
                    if is_wide(wide_mask, obj) {
                        return None;
                    }
                    let ov = regs[obj as usize];
                    let v = match self.py_leaf_attr_get(fid, ip, ov, st)? {
                        LeafAttrGet::Slot { dict_idx, pos, val } => {
                            let mut out = val;
                            if st.n_pending > 0 {
                                for w in st.pending[..st.n_pending].iter().rev() {
                                    if w.dict_idx == dict_idx {
                                        if w.pos == pos {
                                            out = w.val;
                                            break;
                                        }
                                        if w.is_append() {
                                            return None;
                                        }
                                    }
                                }
                            }
                            out
                        }
                        LeafAttrGet::ClassVal { dict_idx, val } => {
                            if st.n_pending > 0
                                && st.pending[..st.n_pending]
                                    .iter()
                                    .any(|w| w.dict_idx == dict_idx && w.is_append())
                            {
                                return None;
                            }
                            val
                        }
                    };
                    regs[dst as usize] = v;
                    clear_wide(&mut wide_mask, dst);
                    ip += 1;
                }
                Instr::PyClassAttr { dst, obj, .. } => {
                    if is_wide(wide_mask, obj) {
                        return None;
                    }
                    let ov = regs[obj as usize];
                    let LeafAttrGet::ClassVal { dict_idx, val } = self.py_leaf_class_attr_get(fid, ip, ov, st)? else {
                        return None;
                    };
                    if st.n_pending > 0
                        && st.pending[..st.n_pending]
                            .iter()
                            .any(|w| w.dict_idx == dict_idx && w.is_append())
                    {
                        return None;
                    }
                    regs[dst as usize] = val;
                    clear_wide(&mut wide_mask, dst);
                    ip += 1;
                }
                Instr::PySetAttr { obj, val, .. } => {
                    if is_wide(wide_mask, obj) || is_wide(wide_mask, val) {
                        return None;
                    }
                    let ov = regs[obj as usize];
                    let vv = regs[val as usize];
                    match self.py_leaf_set_check(fid, ip, ov, depth + 1 < LEAF_MAX_DEPTH, st)? {
                        LeafSetKind::Layout { dict_idx, pos, next_layout, is_append } => {
                            if st.n_pending >= LEAF_MAX_WRITES {
                                return None;
                            }
                            if st.n_pending > 0
                                && st.pending[..st.n_pending]
                                    .iter()
                                    .any(|w| w.dict_idx == dict_idx && (w.is_append() || is_append))
                            {
                                return None;
                            }
                            st.pending[st.n_pending] = PendingAttr {
                                dict_idx,
                                pos,
                                layout_flags: next_layout | if is_append { PENDING_APPEND_BIT } else { 0 },
                                val: vv,
                            };
                            st.n_pending += 1;
                        }
                        LeafSetKind::PropSetter { fid: sfid, fv: sfv } => {
                            let sinfo = self.py_leaf_info(sfid)?;
                            if (sinfo.params as usize) < 2 {
                                return None;
                            }
                            self.py_leaf_dispatch(sfid, sfv, sinfo, &[ov, vv], st, depth + 1)?;
                        }
                    }
                    ip += 1;
                }
                Instr::CallWithThis { dst, callee, this_v, arg_base, argc, .. } => {
                    if argc != 3
                        || is_wide(wide_mask, callee)
                        || is_wide(wide_mask, this_v)
                        || is_wide(wide_mask, arg_base)
                        || is_wide(wide_mask, arg_base + 1)
                        || is_wide(wide_mask, arg_base + 2)
                        || !self.realm_global_objs.is_empty()
                        || !self.realm_fns.is_empty()
                    {
                        return None;
                    }
                    let cv = regs[callee as usize];
                    if !cv.is_heap()
                        || !matches!(self.heap.get(cv.heap_index()), HeapObj::Native(id) if *id == super::super::native::PY_SMFIND)
                    {
                        return None;
                    }
                    let ab = arg_base as usize;
                    let rt_v = regs[this_v as usize];
                    let (found, write_slot) = self.py_smfind_hit(regs[ab], regs[ab + 1], regs[ab + 2], rt_v)?;
                    if let Some(slot) = write_slot {
                        st.pending_mself = Some((rt_v.heap_index(), slot));
                    }
                    regs[dst as usize] = found;
                    clear_wide(&mut wide_mask, dst);
                    ip += 1;
                }
                Instr::PyMethod { dst, obj, .. } => {
                    if is_wide(wide_mask, obj) {
                        return None;
                    }
                    let ov = regs[obj as usize];
                    let LeafAttrGet::ClassVal { dict_idx, val } = self.py_leaf_method_get(fid, ip, ov, st)? else {
                        return None;
                    };
                    if st.n_pending > 0
                        && st.pending[..st.n_pending]
                            .iter()
                            .any(|w| w.dict_idx == dict_idx && w.is_append())
                    {
                        return None;
                    }
                    regs[dst as usize] = val;
                    clear_wide(&mut wide_mask, dst);
                    ip += 1;
                }
                Instr::PushHandler { .. } => {
                    handler_depth += 1;
                    ip += 1;
                }
                Instr::PopHandler => {
                    if handler_depth == 0 {
                        return None;
                    }
                    handler_depth -= 1;
                    ip += 1;
                }
                Instr::PyCall { dst, f, arg_base, argc, .. } => {
                    if depth + 1 >= LEAF_MAX_DEPTH || (argc as usize) > LEAF_MAX_PARAMS || is_wide(wide_mask, f) {
                        return None;
                    }
                    let n = argc as usize;
                    let ab = arg_base as usize;
                    if wide_mask != 0 {
                        for k in 0..n {
                            if is_wide(wide_mask, arg_base + k as u16) {
                                return None;
                            }
                        }
                    }
                    let cfv = regs[f as usize];
                    let (cfid, cinfo) = self.py_leaf_resolve_call(fid, ip, cfv, argc)?;
                    let res = self.py_leaf_dispatch(cfid, cfv, cinfo, &regs[ab..ab + n], st, depth + 1)?;
                    regs[dst as usize] = res;
                    clear_wide(&mut wide_mask, dst);
                    ip += 1;
                }
                Instr::PyGetItem { dst, o, k, seq, dict, .. } => {
                    if wide_mask != 0
                        && (is_wide(wide_mask, o)
                            || is_wide(wide_mask, k)
                            || is_wide(wide_mask, seq)
                            || is_wide(wide_mask, dict))
                    {
                        return None;
                    }
                    let Some(Some(v)) = self
                        .py_get_item(regs[o as usize], regs[k as usize], regs[seq as usize], regs[dict as usize])
                        .ok()?
                    else {
                        return None;
                    };
                    regs[dst as usize] = v;
                    clear_wide(&mut wide_mask, dst);
                    ip += 1;
                }
                Instr::PyLen { dst, v, rt, .. } => {
                    if is_wide(wide_mask, v) || is_wide(wide_mask, rt) {
                        return None;
                    }
                    let n = self.py_len(regs[v as usize], regs[rt as usize])?;
                    regs[dst as usize] = Value::small_bigint(n as i128)?;
                    clear_wide(&mut wide_mask, dst);
                    ip += 1;
                }
                Instr::PyTruth { dst, v, rt, .. } => {
                    if is_wide(wide_mask, v) || is_wide(wide_mask, rt) {
                        return None;
                    }
                    let b = self.py_truth(regs[v as usize], regs[rt as usize])?;
                    regs[dst as usize] = Value::bool(b);
                    clear_wide(&mut wide_mask, dst);
                    ip += 1;
                }
                Instr::Return { src } => {
                    if handler_depth != 0 || is_wide(wide_mask, src) {
                        return None;
                    }
                    return Some(regs[src as usize]);
                }
                _ => return None,
            }
        }
        None
    }

    /// Execute a `PyCall` site whose cache entry `e` names a plain Python
    /// callee (`e.c != 0`) directly via [`Vm::py_leaf_exec`], without pushing
    /// a frame. Used by compiled code (`jit_py_call`).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    #[inline]
    pub(in crate::vm) fn py_leaf_call_site(&mut self, e: &PyIc, f: Value, base: usize, arg_base: u16, argc: u16) -> Option<Value> {
        if e.kind != ic::CALL_LEAF || e.c == 0 || e.a != f.bits() || !f.is_heap() {
            return None;
        }
        let (fid, params) = (e.c as u32, (e.c >> 48) as u16);
        if argc > params || (argc as usize) > LEAF_MAX_PARAMS {
            return None;
        }
        // Quick reject before touching the function record if already known non-leaf.
        if let Some(p) = self.py_rt.as_deref() {
            if matches!(p.leaf_bodies.get(fid as usize), Some(LeafSlot::No)) {
                return None;
            }
        }
        let fi = f.heap_index();
        if self.heap.version_of(fi) != e.hver {
            return None;
        }
        let pos = e.pos as usize;
        match self.heap.get(fi) {
            HeapObj::Object(m) if pos < m.slot_len() && !m.is_accessor_at(pos) && m.val_at(pos).bits() == e.b => {}
            _ => return None,
        }
        self.py_leaf_exec_window(fid, f, base, arg_base, argc)
    }

    /// Execute a pure small-int self-recursive Python function `fid` via its
    /// compiled x86-64 micro-JIT body (`jit::py_rec`), returning `Some(val)`
    /// on completion or `None` (zero side effects) to fall back to the
    /// interpreter.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    #[inline(never)]
    pub(crate) fn py_rec_exec(&mut self, fid: u32, fv: Value, args: &[Value]) -> Option<Value> {
        use crate::codegen::{compile_py_rec, py_rec_enabled, PyRecCtx, PyRecSlot, REC_MAX_DEPTH};
        if !py_rec_enabled()
            || super::prof_py::on()
            || self.frames.len() >= 16
            || !self.native_kernel_admits(64, 0)
            || args.is_empty()
            || args.len() > 4
        {
            return None;
        }
        let f = fid as usize;
        // Compile on first sight (cached in `PyRt::rec_jits`).
        let needs_compile = matches!(
            self.py_rt.as_deref()?.rec_jits.get(f),
            None | Some(PyRecSlot::Unread)
        );
        if needs_compile {
            let compiled = compile_py_rec(self.func(f));
            let slot = match compiled {
                Some(c) => PyRecSlot::Yes(Box::new(c)),
                None => PyRecSlot::No,
            };
            let p = self.py_rt.as_deref_mut()?;
            if p.rec_jits.len() <= f {
                p.rec_jits.resize_with(f + 1, PyRecSlot::default);
            }
            p.rec_jits[f] = slot;
        }
        let (entry, params, global_ips, n_globals, rt) = {
            let p = self.py_rt.as_deref()?;
            let Some(PyRecSlot::Yes(rec)) = p.rec_jits.get(f) else {
                return None;
            };
            (rec.entry, rec.params as usize, rec.global_ips, rec.n_globals as usize, p.r)
        };
        if args.len() != params {
            return None;
        }
        // Unbox all arguments as small immediate BigInts first (cheap reject).
        let mut a = [0i64; 4];
        for i in 0..params {
            a[i] = args[i].small_bigint_val()?;
        }
        // Verify every self-call global binding still resolves to `fv` (exact
        // semantics even if the function name is rebound in module globals).
        for &gip in &global_ips[..n_globals] {
            let gip = gip as usize;
            let bound = match self.py_global_hit(fid, gip, fv, rt) {
                Some(v) => v,
                None => {
                    let key = match self.func(f).code.get(gip) {
                        Some(&Instr::PyGlobal { key, .. }) => key,
                        _ => return None,
                    };
                    self.py_global(fid, gip, fv, rt, key)?
                }
            };
            if bound.bits() != fv.bits() {
                return None;
            }
        }
        let init_budget = self
            .instr_rec
            .as_ref()
            .and_then(|r| r.finite_remaining(self.meter_hot()))
            .map(|left| left.min(100_000_000) as i64)
            .unwrap_or(100_000_000);
        if init_budget <= 64 {
            return None;
        }
        let mut ctx = PyRecCtx {
            budget: init_budget,
            depth_left: REC_MAX_DEPTH,
        };
        let res = entry(a[0], a[1], a[2], a[3], &mut ctx);
        if res == i64::MIN {
            return None;
        }
        let boxed = Value::small_bigint(res as i128)?;
        let spent = (init_budget - ctx.budget).max(1);
        self.charge_steps(spent);
        Some(boxed)
    }

    /// Compile and cache `rec_jits[fid]` if not yet attempted; returns whether
    /// `fid` has a compiled recursive micro-JIT body.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn py_rec_prepare(&mut self, fid: u32) -> bool {
        use crate::codegen::{compile_py_rec, py_rec_enabled, PyRecSlot};
        if !py_rec_enabled() {
            return false;
        }
        let f = fid as usize;
        let known = self.py_rt.as_deref().and_then(|p| p.rec_jits.get(f));
        match known {
            Some(PyRecSlot::Yes(_)) => return true,
            Some(PyRecSlot::No) => return false,
            _ => {}
        }
        let compiled = compile_py_rec(self.func(f));
        let yes = compiled.is_some();
        let slot = match compiled {
            Some(c) => PyRecSlot::Yes(Box::new(c)),
            None => PyRecSlot::No,
        };
        if let Some(p) = self.py_rt.as_deref_mut() {
            if p.rec_jits.len() <= f {
                p.rec_jits.resize_with(f + 1, PyRecSlot::default);
            }
            p.rec_jits[f] = slot;
        }
        yes
    }

    /// Execute a `PyCall` site via [`Vm::py_rec_exec`] when its cache entry
    /// names a plain Python callee (`e.c != 0`).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    #[inline]
    pub(in crate::vm) fn py_rec_call_site(&mut self, e: &PyIc, f: Value, base: usize, arg_base: u16, argc: u16) -> Option<Value> {
        if e.kind != ic::CALL_REC || e.c == 0 || e.a != f.bits() || !f.is_heap() || argc == 0 || argc > 4 {
            return None;
        }
        let (fid, params) = (e.c as u32, (e.c >> 48) as u16);
        if argc != params {
            return None;
        }
        if let Some(p) = self.py_rt.as_deref() {
            if matches!(p.rec_jits.get(fid as usize), Some(crate::codegen::PyRecSlot::No)) {
                return None;
            }
        }
        let fi = f.heap_index();
        if self.heap.version_of(fi) != e.hver {
            return None;
        }
        let pos = e.pos as usize;
        match self.heap.get(fi) {
            HeapObj::Object(m) if pos < m.slot_len() && !m.is_accessor_at(pos) && m.val_at(pos).bits() == e.b => {}
            _ => return None,
        }
        let mut argv = [Value::UNDEFINED; 4];
        let start = base + arg_base as usize;
        let n = argc as usize;
        argv[..n].copy_from_slice(&self.regs[start..start + n]);
        self.py_rec_exec(fid, f, &argv[..n])
    }
}
