//! The lite loop: a second, small interpreter loop for simple functions.
//!
//! `dispatch_body` is one function of a few hundred instruction arms. Its
//! hot state does not fit the registers (`self` itself lives on the stack),
//! and any edit to it moves its code and register allocation enough to swing
//! unrelated rows by 5-10%. A recursive integer function such as `fib`
//! spends a fifth of its time in the frame push alone and needs nothing but
//! a dozen instructions; this loop runs exactly those, in a function small
//! enough to keep its state in registers.
//!
//! It works on the real frames and the real register file, so leaving it is
//! free: on anything it does not run (an instruction outside its set, a
//! callee neither of its caches answers, a frame with handlers, a global it
//! must resolve the slow way) it writes the frame's `ip` back and returns,
//! and the main loop executes that same instruction. Exceptions propagate as
//! from the main loop, the frames being exactly what it would have left.
//!
//! It dispatches each function's superinstruction copy (`FusedCode`) when
//! the main loop would, positions being the original code's, so a hand-over
//! at any `ip` lands where the main loop expects.
//!
//! A function runs here only when every instruction of it, fused copy
//! included, is in the set (`lite_eligible`), so the hand-over is rare; a
//! call into, or a return to, any other function hands it over too. Only an
//! interpreter-only VM without a recorder or the per-frame statistics uses it
//! (`lite_entry_ok`).

use super::*;
use crate::bytecode::Instr;

/// How the lite loop ended.
pub(crate) enum LiteExit {
    /// The `stop_depth` frame returned this value.
    Done(Value),
    /// The top frame's `ip` is where the main loop takes over.
    Resume,
}

/// `ZIPP_NO_LITE=1`: every frame runs in the main loop (for A/B).
#[inline]
fn lite_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static STATE: AtomicU8 = AtomicU8::new(0);
    match STATE.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let on = std::env::var_os("ZIPP_NO_LITE").is_none();
            STATE.store(if on { 1 } else { 2 }, Ordering::Relaxed);
            on
        }
    }
}

/// The instructions the lite loop runs, the interpreter's superinstructions
/// for them included (it dispatches the function's `FusedCode` copy).
fn lite_instr(i: &Instr) -> bool {
    matches!(
        i,
        Instr::LoadInt { .. }
            | Instr::Move { .. }
            | Instr::LoadGlobal { .. }
            | Instr::AddInt { .. }
            | Instr::Add { .. }
            | Instr::JumpIfNotLt { .. }
            | Instr::JumpIfNotLe { .. }
            | Instr::Jump { .. }
            | Instr::Call { .. }
            | Instr::Return { .. }
            | Instr::ReturnUndefined
            | Instr::FuseIntCmpJump { .. }
            | Instr::FuseGlobalCmpJump { .. }
            | Instr::FuseLoadGlobalAddInt { .. }
            | Instr::FuseAddReturn { .. }
            | Instr::FuseAddLoadInt { .. }
            | Instr::FuseAddIntMove { .. }
            | Instr::FuseLoadGlobal2 { .. }
            // A loop's: element and property reads and stores through the
            // caches, the bitwise ops, strict equality and the branches on
            // it, and the counter step that closes a loop.
            | Instr::Sub { .. }
            | Instr::Mul { .. }
            | Instr::Bitwise { .. }
            | Instr::FuseLoadIntBitwise { .. }
            | Instr::Eq { .. }
            | Instr::Ne { .. }
            | Instr::FuseEqJump { .. }
            | Instr::JumpIfFalse { .. }
            | Instr::JumpIfTrue { .. }
            | Instr::FuseAddIntJump { .. }
            | Instr::GetIndex { .. }
            | Instr::GetProp { .. }
            | Instr::SetProp { .. }
    )
}

/// Hand-overs a function may take before it stays in the main loop for
/// good. A hand-over sends the rest of the activation to the main loop, to
/// its next frame transition; a loop that does that on every iteration (a
/// call to a function the lite loop does not run, a cache that never
/// answers) would pay both loops' transitions each time.
const LITE_HANDOVER_BUDGET: u16 = 64;

/// Whether `code` has a loop: a jump to its own position or an earlier one.
#[cfg(test)]
fn has_back_edge(code: &[Instr]) -> bool {
    code.iter().enumerate().any(|(ip, i)| {
        let target = match *i {
            Instr::Jump { target }
            | Instr::JumpIfFalse { target, .. }
            | Instr::JumpIfTrue { target, .. }
            | Instr::JumpIfNotLt { target, .. }
            | Instr::JumpIfNotLe { target, .. }
            | Instr::FuseAddIntJump { target, .. } => target,
            _ => return false,
        };
        target as usize <= ip
    })
}

/// Slots of the lite loop's callee cache, indexed by the callee's heap slot.
const LITE_CALL_SLOTS: usize = 256;

/// One slot of the lite loop's callee cache: the callee's bits and version
/// when `ic_call` resolved it to a plain user function the lite loop runs,
/// and that resolution. What a Func or Closure object calls never changes
/// and a reused slot has a new version, so it depends on the callee alone.
#[derive(Clone, Copy)]
struct LiteCall {
    bits: u64,
    ver: u32,
    fid: u32,
    closure: u32,
}

impl LiteCall {
    const EMPTY: LiteCall = LiteCall {
        bits: u64::MAX,
        ver: 0,
        fid: 0,
        closure: 0,
    };
}

/// The lite loop's per-VM tables.
pub(crate) struct LiteState<'p> {
    /// Per function id: 0 not yet decided, 1 the lite loop runs it, 2 not.
    ok: Vec<u8>,
    /// Per function id the lite loop runs: what it dispatches (`lite_code`),
    /// found once so a frame transition is one load.
    code: Vec<&'p [Instr]>,
    /// Per function id the lite loop runs: the hand-overs it has left
    /// (`LITE_HANDOVER_BUDGET`).
    budget: Vec<u16>,
    calls: Box<[LiteCall]>,
}

impl Default for LiteState<'_> {
    fn default() -> Self {
        LiteState {
            ok: Vec::new(),
            code: Vec::new(),
            budget: Vec::new(),
            calls: vec![LiteCall::EMPTY; LITE_CALL_SLOTS].into_boxed_slice(),
        }
    }
}

impl<'p> Vm<'p> {
    /// Whether this VM may run frames in the lite loop now: no native tier
    /// to enter at a fresh frame, no recorder counting instructions, no
    /// per-frame statistics.
    #[inline(always)]
    pub(crate) fn lite_entry_ok(&self) -> bool {
        #[cfg(all(feature = "jit", any(target_arch = "x86_64", target_arch = "aarch64")))]
        if self.jit_enabled {
            return false;
        }
        #[cfg(feature = "instrument")]
        if self.instr_rec.is_some() {
            return false;
        }
        #[cfg(not(target_arch = "wasm32"))]
        if super::dispatch::interp_stats::enabled() {
            return false;
        }
        true
    }

    /// Whether the lite loop runs function `fid` (decided once per function
    /// by `lite_decide`).
    #[inline(always)]
    pub(crate) fn lite_eligible(&mut self, fid: u32) -> bool {
        match self.lite.ok.get(fid as usize) {
            Some(&1) => true,
            Some(&2) => false,
            _ => self.lite_decide(fid),
        }
    }

    /// Every instruction of `fid`, in its code and its superinstruction copy,
    /// is one the lite loop runs, and its frames need none of `setup_call`'s
    /// rarer work.
    #[cold]
    #[inline(never)]
    fn lite_decide(&mut self, fid: u32) -> bool {
        let i = fid as usize;
        let p = self.func(i);
        let ok = lite_enabled()
            && !p.is_generator
            && !p.is_async
            && !p.lexical_this
            && p.rest_reg.is_none()
            && p.arguments_reg.is_none()
            && p.code.iter().all(lite_instr)
            && Self::lite_code(p).iter().all(lite_instr)
            // A private member's read or write is the main arm's before any
            // cache (brand checks, the private side table).
            && !p.code.iter().any(|i| match *i {
                Instr::GetProp { name, .. } | Instr::SetProp { name, .. } => {
                    is_private_key(&p.string_constants[name as usize])
                }
                _ => false,
            });
        if self.lite.ok.len() <= i {
            self.lite.ok.resize(i + 1, 0);
            self.lite.code.resize(i + 1, &[]);
            self.lite.budget.resize(i + 1, 0);
        }
        self.lite.ok[i] = if ok { 1 } else { 2 };
        if ok {
            self.lite.code[i] = Self::lite_code(p);
            self.lite.budget[i] = LITE_HANDOVER_BUDGET;
        }
        ok
    }

    /// `fid` handed its frame to the main loop: one less hand-over left,
    /// and with none left the main loop runs it from now on.
    #[cold]
    #[inline(never)]
    fn lite_handed_over(&mut self, fid: u32) {
        let b = &mut self.lite.budget[fid as usize];
        *b = b.saturating_sub(1);
        if *b == 0 {
            self.lite.ok[fid as usize] = 2;
        }
    }

    /// What the lite loop dispatches for `fid`, which it runs.
    #[inline(always)]
    fn lite_fn_code(&self, fid: u32) -> &'p [Instr] {
        self.lite.code[fid as usize]
    }

    /// What the lite loop dispatches for a function: its superinstruction
    /// copy where the main loop would use one (see `interp_fuse_ok`; the
    /// lite loop never runs under a recorder), else its code.
    #[inline(always)]
    fn lite_code(p: &'p crate::bytecode::FuncProto) -> &'p [Instr] {
        if crate::bytecode::interp_fuse_enabled() {
            p.fused.entered(&p.code).unwrap_or(p.code.as_slice())
        } else {
            p.code.as_slice()
        }
    }

    /// The callee `callee` resolves to for a plain call from `func_id`'s
    /// `ip`: the lite cache's answer, else `ic_call`'s (remembered when the
    /// lite loop runs the callee).
    #[inline(always)]
    fn lite_resolve(&mut self, func_id: u32, ip: usize, callee: Value) -> Option<(u32, u32)> {
        if callee.is_heap() {
            let idx = callee.heap_index();
            let e = self.lite.calls[idx as usize & (LITE_CALL_SLOTS - 1)];
            if e.bits == callee.bits() && e.ver == self.heap.version_of(idx) {
                return Some((e.fid, e.closure));
            }
        }
        let call = self.ic_call(func_id, ip, callee)?;
        if self.lite_eligible(call.fid) {
            let idx = callee.heap_index();
            self.lite.calls[idx as usize & (LITE_CALL_SLOTS - 1)] = LiteCall {
                bits: callee.bits(),
                ver: self.heap.version_of(idx),
                fid: call.fid,
                closure: call.closure,
            };
        }
        Some((call.fid, call.closure))
    }

    /// Run the top frame, and the eligible frames it calls or returns to, in
    /// the lite loop (see the module doc).
    #[inline(never)]
    pub(crate) fn run_lite(&mut self, stop_depth: usize) -> Result<LiteExit, Thrown> {
        let mut top = self.frames.len() - 1;
        let mut func_id = self.frames[top].func;
        let mut base = self.frames[top].base;
        let mut ip = self.frames[top].ip;
        let mut code: &'p [Instr] = self.lite_fn_code(func_id);
        // Hand the instruction at `ip` to the main loop.
        macro_rules! hand_over {
            () => {{
                self.frames[top].ip = ip;
                self.lite_handed_over(func_id);
                return Ok(LiteExit::Resume);
            }};
        }
        // A global's value, or the main loop's slow path for it.
        macro_rules! global {
            ($idx:expr) => {{
                let v = self.globals[$idx as usize];
                if v.is_uninitialized() || (self.global_route_epoch != 0 && self.global_real_own_route($idx)) {
                    hand_over!();
                }
                v
            }};
        }
        // Return `v` from the top frame: done, on to the caller here, or to
        // the main loop.
        macro_rules! ret {
            ($v:expr) => {{
                let v = $v;
                if self.pop_frame_fast(v, stop_depth) {
                    return Ok(LiteExit::Done(v));
                }
                self.maybe_gc();
                top = self.frames.len() - 1;
                func_id = self.frames[top].func;
                if !self.lite_eligible(func_id) {
                    return Ok(LiteExit::Resume);
                }
                base = self.frames[top].base;
                ip = self.frames[top].ip;
                code = self.lite_fn_code(func_id);
            }};
        }
        loop {
            match code[ip] {
                Instr::LoadInt { dst, val } => {
                    self.set(base, dst, Value::int(val));
                    ip += 1;
                }
                Instr::Move { dst, src } => {
                    let v = self.get(base, src);
                    self.set(base, dst, v);
                    ip += 1;
                }
                Instr::LoadGlobal { dst, idx } => {
                    let v = global!(idx);
                    self.set(base, dst, v);
                    ip += 1;
                }
                Instr::FuseLoadGlobal2 { dst, idx, dst2, idx2 } => {
                    let v = global!(idx);
                    let v2 = global!(idx2);
                    self.set(base, dst, v);
                    self.set(base, dst2, v2);
                    ip += 2;
                }
                Instr::AddInt { dst, a, imm, upd } => {
                    let va = self.get(base, a);
                    let r = self.add_int_value(va, imm, upd)?;
                    self.set(base, dst, r);
                    ip += 1;
                }
                Instr::FuseAddIntMove { dst, a, imm, upd, mdst, msrc } => {
                    let va = self.get(base, a);
                    let r = self.add_int_value(va, imm, upd)?;
                    self.set(base, dst, r);
                    let v = self.get(base, msrc);
                    self.set(base, mdst, v);
                    ip += 2;
                }
                Instr::FuseLoadGlobalAddInt { gdst, idx, dst, a, imm, upd } => {
                    let g = global!(idx);
                    self.set(base, gdst, g);
                    let va = self.get(base, a);
                    let r = self.add_int_value(va, imm, upd)?;
                    self.set(base, dst, r);
                    ip += 2;
                }
                Instr::Add { dst, a, b } => {
                    let r = self.add(base, a, b)?;
                    self.set(base, dst, r);
                    ip += 1;
                }
                Instr::FuseAddLoadInt { ldst, val, dst, a } => {
                    self.set(base, ldst, Value::int(val));
                    let va = self.get(base, a);
                    let r = if va.is_int() {
                        match va.as_int().checked_add(val) {
                            Some(v) => Value::int(v),
                            None => Value::num(va.as_int() as f64 + val as f64),
                        }
                    } else if va.is_double() {
                        Value::num(va.as_f64() + val as f64)
                    } else {
                        self.add_values(va, Value::int(val))?
                    };
                    self.set(base, dst, r);
                    ip += 2;
                }
                Instr::JumpIfNotLt { a, b, target } => {
                    let (va, vb) = (self.get(base, a), self.get(base, b));
                    let lt = if va.is_int() && vb.is_int() {
                        va.as_int() < vb.as_int()
                    } else {
                        // The main arm's small-BigInt path answers what
                        // `cmp_lt` answers; take it there.
                        #[cfg(feature = "python")]
                        if va.is_small_bigint() && vb.is_small_bigint() {
                            hand_over!();
                        }
                        self.cmp_lt(base, a, b, true)?
                    };
                    ip = if lt { ip + 1 } else { target as usize };
                }
                Instr::JumpIfNotLe { a, b, target } => {
                    let (va, vb) = (self.get(base, a), self.get(base, b));
                    let le = if va.is_int() && vb.is_int() {
                        va.as_int() <= vb.as_int()
                    } else {
                        self.cmp_le(base, a, b, true)?
                    };
                    ip = if le { ip + 1 } else { target as usize };
                }
                Instr::FuseIntCmpJump { ldst, val, a, le, target } => {
                    self.set(base, ldst, Value::int(val));
                    let va = self.get(base, a);
                    let r = if va.is_int() {
                        if le {
                            va.as_int() <= val
                        } else {
                            va.as_int() < val
                        }
                    } else if le {
                        self.cmp_le(base, a, ldst, true)?
                    } else {
                        self.cmp_lt(base, a, ldst, true)?
                    };
                    ip = if r { ip + 2 } else { target as usize };
                }
                Instr::FuseGlobalCmpJump { gdst, idx, a, le, target } => {
                    let g = global!(idx);
                    self.set(base, gdst, g);
                    let va = self.get(base, a);
                    let r = if va.is_int() && g.is_int() {
                        if le {
                            va.as_int() <= g.as_int()
                        } else {
                            va.as_int() < g.as_int()
                        }
                    } else if le {
                        self.cmp_le(base, a, gdst, true)?
                    } else {
                        self.cmp_lt(base, a, gdst, true)?
                    };
                    ip = if r { ip + 2 } else { target as usize };
                }
                Instr::Jump { target } => {
                    let t = target as usize;
                    // A back-edge: the main arm's GC safe point.
                    if t < ip {
                        self.maybe_gc();
                    }
                    ip = t;
                }
                Instr::FuseAddIntJump { dst, a, imm, upd, target } => {
                    let va = self.get(base, a);
                    let r = self.add_int_value(va, imm, upd)?;
                    self.set(base, dst, r);
                    // The back-edge `Jump` at `ip + 1`: its GC safe point.
                    self.maybe_gc();
                    ip = target as usize;
                }
                Instr::JumpIfFalse { cond, target } => {
                    let v = self.get(base, cond);
                    let t = if v.is_bool() {
                        v.bits() == Value::TRUE.bits()
                    } else {
                        self.truthy(v)
                    };
                    if !t {
                        // A taken backward branch closes a loop (`do … while`).
                        if (target as usize) < ip {
                            self.maybe_gc();
                        }
                        ip = target as usize;
                    } else {
                        ip += 1;
                    }
                }
                Instr::JumpIfTrue { cond, target } => {
                    let v = self.get(base, cond);
                    let t = if v.is_bool() {
                        v.bits() == Value::TRUE.bits()
                    } else {
                        self.truthy(v)
                    };
                    if t {
                        if (target as usize) < ip {
                            self.maybe_gc();
                        }
                        ip = target as usize;
                    } else {
                        ip += 1;
                    }
                }
                Instr::Sub { dst, a, b } => {
                    let (va, vb) = (self.get(base, a), self.get(base, b));
                    let r = if va.is_int() && vb.is_int() {
                        match va.as_int().checked_sub(vb.as_int()) {
                            Some(v) => Value::int(v),
                            None => Value::num(va.as_int() as f64 - vb.as_int() as f64),
                        }
                    } else if va.is_number() && vb.is_number() {
                        Value::num(va.as_f64() - vb.as_f64())
                    } else {
                        self.numeric_binop(super::helpers_misc::BigOp::Sub, va, vb)?
                    };
                    self.set(base, dst, r);
                    ip += 1;
                }
                Instr::Mul { dst, a, b } => {
                    let (va, vb) = (self.get(base, a), self.get(base, b));
                    let r = if va.is_int() && vb.is_int() {
                        // A zero product with a negative operand is -0 (see
                        // the main arm).
                        let (ia, ib) = (va.as_int(), vb.as_int());
                        match ia.checked_mul(ib) {
                            Some(v) if v != 0 || (ia | ib) >= 0 => Value::int(v),
                            _ => Value::num(ia as f64 * ib as f64),
                        }
                    } else if va.is_number() && vb.is_number() {
                        Value::num(va.as_f64() * vb.as_f64())
                    } else {
                        self.numeric_binop(super::helpers_misc::BigOp::Mul, va, vb)?
                    };
                    self.set(base, dst, r);
                    ip += 1;
                }
                Instr::Bitwise { dst, a, b, op } => {
                    let (va, vb) = (self.get(base, a), self.get(base, b));
                    let r = self.fused_bitwise(va, vb, op)?;
                    self.set(base, dst, r);
                    ip += 1;
                }
                Instr::FuseLoadIntBitwise { ldst, val, dst, a, b, op } => {
                    self.set(base, ldst, Value::int(val));
                    let (va, vb) = (self.get(base, a), self.get(base, b));
                    let r = self.fused_bitwise(va, vb, op)?;
                    self.set(base, dst, r);
                    ip += 2;
                }
                Instr::Eq { dst, a, b } => {
                    let (va, vb) = (self.get(base, a), self.get(base, b));
                    let r = if va.is_int() && vb.is_int() {
                        va.bits() == vb.bits()
                    } else {
                        self.strict_eq(base, a, b)
                    };
                    self.set(base, dst, Value::bool(r));
                    ip += 1;
                }
                Instr::Ne { dst, a, b } => {
                    let (va, vb) = (self.get(base, a), self.get(base, b));
                    let r = if va.is_int() && vb.is_int() {
                        va.bits() == vb.bits()
                    } else {
                        self.strict_eq(base, a, b)
                    };
                    self.set(base, dst, Value::bool(!r));
                    ip += 1;
                }
                Instr::FuseEqJump { dst, a, b, ne, when, target } => {
                    let (va, vb) = (self.get(base, a), self.get(base, b));
                    let eq = if va.is_int() && vb.is_int() {
                        va.bits() == vb.bits()
                    } else {
                        self.strict_eq(base, a, b)
                    };
                    let r = eq != ne;
                    self.set(base, dst, Value::bool(r));
                    // Forward only (see the variant).
                    ip = if r == when { target as usize } else { ip + 2 };
                }
                Instr::GetIndex { dst, obj, key } => {
                    let (o, k) = (self.get(base, obj), self.get(base, key));
                    // The main arm's two leaves, in its order: a numeric
                    // typed array's element, then a present dense element.
                    if o.is_heap() {
                        let ta_idx = o.heap_index();
                        if let HeapObj::TypedArray {
                            buffer,
                            kind,
                            byte_offset,
                            length,
                        } = self.heap.get(ta_idx)
                        {
                            if native::TA_KINDS[*kind as usize].2 {
                                hand_over!();
                            }
                            let Some(i) = array_index(k) else {
                                hand_over!();
                            };
                            let view = crate::vm::typedarray::TaInterpNumericView::from_matched_non_bigint(
                                ta_idx,
                                *buffer,
                                *kind,
                                *byte_offset,
                                *length,
                            );
                            let Some(r) = self.ta_interp_numeric_get_view(view, i) else {
                                hand_over!();
                            };
                            self.set(base, dst, r);
                            ip += 1;
                            continue;
                        }
                    }
                    let Some(r) = self.interp_dense_int_get(o, k) else {
                        hand_over!();
                    };
                    self.set(base, dst, r);
                    ip += 1;
                }
                Instr::GetProp { dst, obj, name } => {
                    let o = self.get(base, obj);
                    if let Some(v) = self.ic_get_prop_own_hit(func_id, ip, o) {
                        self.set(base, dst, v);
                        ip += 1;
                        continue;
                    }
                    // The caches answer only plain objects, for which the
                    // main arm's steps before its cache (a string's, an
                    // array's or a typed array's `length`, a shapeless
                    // receiver's intrinsic) answer nothing, and a function
                    // here names no private member (`lite_decide`). Any
                    // other receiver, and anything the cache does not
                    // answer with a value, is the main loop's.
                    if !o.is_heap() || !matches!(self.heap.get(o.heap_index()), HeapObj::Object(_)) {
                        hand_over!();
                    }
                    let key: &'p str = self.func(func_id as usize).string_constants[name as usize].as_str();
                    match self.ic_get_prop(func_id, ip, o, key) {
                        GetAct::Value(v) => {
                            self.set(base, dst, v);
                            ip += 1;
                        }
                        _ => hand_over!(),
                    }
                }
                Instr::SetProp { obj, val, .. } => {
                    let o = self.get(base, obj);
                    let v = self.get(base, val);
                    // The cache's own-data hit, the main arm's first step for
                    // a name that is not private (`lite_decide`).
                    if !self.ic_set_prop_own_hit(func_id, ip, o, v) {
                        hand_over!();
                    }
                    ip += 1;
                }
                Instr::Call {
                    dst,
                    callee,
                    arg_base,
                    argc,
                } => {
                    let callee_v = self.get(base, callee);
                    let Some((fid, closure)) = self.lite_resolve(func_id, ip, callee_v) else {
                        hand_over!();
                    };
                    self.setup_call_lean(
                        fid,
                        closure,
                        Value::UNDEFINED,
                        base,
                        arg_base,
                        argc,
                        dst,
                        ip + 1,
                        callee_v,
                    )?;
                    // The main loop's frame-transition safe point.
                    self.maybe_gc();
                    if !self.lite_eligible(fid) {
                        // The callee runs in the main loop, and the caller
                        // after it until its next frame transition.
                        self.lite_handed_over(func_id);
                        return Ok(LiteExit::Resume);
                    }
                    top = self.frames.len() - 1;
                    func_id = fid;
                    base = self.frames[top].base;
                    ip = 0;
                    code = self.lite_fn_code(func_id);
                }
                Instr::Return { src } => {
                    if !self.frames[top].handlers.is_empty() {
                        hand_over!();
                    }
                    ret!(self.get(base, src));
                }
                Instr::FuseAddReturn { dst, a, b, src } => {
                    let r = self.add(base, a, b)?;
                    self.set(base, dst, r);
                    if !self.frames[top].handlers.is_empty() {
                        // The `Return` at `ip + 1` routes the `finally`.
                        ip += 1;
                        hand_over!();
                    }
                    ret!(self.get(base, src));
                }
                Instr::ReturnUndefined => {
                    if !self.frames[top].handlers.is_empty() {
                        hand_over!();
                    }
                    ret!(Value::UNDEFINED);
                }
                _ => hand_over!(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A loop runs in the lite loop. One that hands over on every iteration
    /// (here a call to a function the lite loop does not run) runs there only
    /// for its hand-over budget, then in the main loop for good.
    #[test]
    fn loops_run_here_until_they_hand_over_too_often() {
        let src = "function loopSum(n) { var s = 0; for (var i = 0; i < n; i++) { s = s + i; } return s; } \
                   function other(o) { return typeof o === 'number' ? 1 : 0; } \
                   function callsOut(n) { var s = 0; for (var i = 0; i < n; i++) { s = s + other(i); } return s; } \
                   function fib(n) { return n < 2 ? n : fib(n - 1) + fib(n - 2); } \
                   console.log(loopSum(10) + callsOut(200) + fib(10));";
        let ast = crate::front::parse_auto(src).expect("parse");
        let program = crate::compile::compile_program(&ast, src).expect("compile");
        let fid = |name: &str| program.functions.iter().position(|f| f.name == name).expect(name) as u32;
        let (lp, out, fb) = (fid("loopSum"), fid("callsOut"), fid("fib"));
        assert!(has_back_edge(&program.functions[lp as usize].code), "loopSum has a loop");
        let mut vm = Vm::new(&program);
        #[cfg(all(feature = "jit", any(target_arch = "x86_64", target_arch = "aarch64")))]
        vm.set_jit_enabled(false);
        vm.run().expect("run");
        assert_eq!(vm.output, ["300"]);
        if lite_enabled() {
            assert!(vm.lite_eligible(lp), "loopSum runs in the lite loop");
            assert!(vm.lite_eligible(fb), "fib runs in the lite loop");
            assert!(!vm.lite_eligible(fid("other")), "typeof is not in the lite set");
            // Eligible by its instructions, so only the budget sent it back.
            let p = &program.functions[out as usize];
            assert!(p.code.iter().chain(Vm::lite_code(p)).all(lite_instr), "callsOut is in the lite set");
            assert_eq!(vm.lite.ok[out as usize], 2, "callsOut spent its hand-overs");
            assert_eq!(vm.lite.budget[out as usize], 0);
        }
    }
}