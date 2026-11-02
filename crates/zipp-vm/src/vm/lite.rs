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

/// How one copy of the lite loop ended (`run_lite_in`).
enum LiteStep {
    Exit(LiteExit),
    /// The top frame is a full-class function: run it in the full copy.
    Full,
}

/// What an object method's call made of its site (`Vm::lite_call_method`).
enum LiteCallOut {
    /// A leaf's result, its frameless run done.
    Value(Value),
    /// The callee's frame, pushed.
    Pushed(u32),
    /// Not the method cache's: the main loop's.
    HandOver,
}

/// A function's class in the lite loop (`LiteState::ok`).
const LITE_UNDECIDED: u8 = 0;
/// Runs in the small copy: only `lite_small_instr` instructions.
const LITE_SMALL: u8 = 1;
/// The main loop's.
const LITE_NO: u8 = 2;
/// Runs in the full copy.
const LITE_FULL: u8 = 3;

env_off_switch! {
    /// `ZIPP_NO_LITE=1`: every frame runs in the main loop (for A/B).
    #[inline]
    fn lite_enabled() = "ZIPP_NO_LITE"
}

env_off_switch! {
    /// `ZIPP_NO_LITE_ASYNC=1`: no async function (and no module's top level)
    /// runs in the lite loop, for A/B.
    #[inline]
    fn lite_async_enabled() = "ZIPP_NO_LITE_ASYNC"
}

env_off_switch! {
    /// `ZIPP_NO_PARTIAL_LITE=1`: only a function that is all lite instructions
    /// runs in the lite loop (`loops_are_lite`), for A/B.
    #[inline]
    fn partial_lite_enabled() = "ZIPP_NO_PARTIAL_LITE"
}

/// The instructions of the lite loop's small copy, the superinstructions over
/// them included: a recursive integer function's (`fib`). The loop is
/// compiled twice (`run_lite_in`) because each arm added for loops and
/// property code cost such a function's calls in register allocation: fib
/// ran 1.16x QuickJS with only these and 1.25x with every arm.
fn lite_small_instr(i: &Instr) -> bool {
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
            | Instr::FuseMove2 { .. }
    )
}

/// The instructions the lite loop runs (its full copy), the interpreter's
/// superinstructions for them included (it dispatches the function's
/// `FusedCode` copy).
fn lite_instr(i: &Instr) -> bool {
    lite_small_instr(i)
        || matches!(
            i,
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
            | Instr::FuseIntEqJump { .. }
            | Instr::FuseIntSwitch { .. }
            | Instr::FuseIntNeJump { .. }
            | Instr::JumpIfFalse { .. }
            | Instr::JumpIfTrue { .. }
            | Instr::FuseAddIntJump { .. }
            | Instr::GetIndex { .. }
            | Instr::GetProp { .. }
            | Instr::SetProp { .. }
            // A closure's: its captured variables, an absent argument's
            // default, a `Math` method.
            | Instr::LoadUndefined { .. }
            | Instr::UpvalGet { .. }
            | Instr::FuseUpvalGetMove { .. }
            | Instr::UpvalSet { .. }
            | Instr::MathOp { .. }
            // A script's counters and accumulators.
            | Instr::StoreGlobal { .. }
            | Instr::StoreGlobalStrict { .. }
            | Instr::StoreGlobalResolved { .. }
            // What most loops' bodies held besides those (`loops_are_lite`
            // found them blocking the most loops of the suites): method and
            // member calls, constants, element stores, the remainder and the
            // comparisons, a function's own captured cells.
            | Instr::CallMethod { .. }
            | Instr::CallWithThis { .. }
            | Instr::LoadConst { .. }
            | Instr::LoadNull { .. }
            | Instr::LoadBool { .. }
            | Instr::SetIndex { .. }
            | Instr::Mod { .. }
            | Instr::Neg { .. }
            | Instr::Not { .. }
            | Instr::Lt { .. }
            | Instr::Le { .. }
            | Instr::Gt { .. }
            | Instr::Ge { .. }
            | Instr::CellGet { .. }
            | Instr::CellSet { .. }
    )
}

/// Hand-overs a function may take before it stays in the main loop for
/// good. A hand-over sends the rest of the activation to the main loop, to
/// its next frame transition; a loop that does that on every iteration (a
/// call to a function the lite loop does not run, a cache that never
/// answers) would pay both loops' transitions each time.
const LITE_HANDOVER_BUDGET: u16 = 64;

/// Instructions an entry into the lite loop must run, before it returns to
/// a caller in the main loop, to pay for entering and leaving it. A shorter
/// entry spends one of the entered function's hand-overs: a small function a
/// main-loop caller calls millions of times (a closure handed to a loop) ran
/// 13% slower in the lite loop than in the main loop.
const LITE_MIN_ENTRY_STEPS: u64 = 128;

/// What a loop iteration (a back-edge) and a call add to an entry's step
/// count (`LITE_MIN_ENTRY_STEPS`), in place of one per instruction: the
/// count lived on the stack, and its increment was a read-modify-write of
/// memory before every instruction this loop ran. A straight-line function
/// still counts nothing, which is the case the threshold is there for.
const LITE_LOOP_STEPS: u64 = 16;
const LITE_CALL_STEPS: u64 = 8;

/// The target of `i` when it is a jump the lite loop or the compiler's loops
/// can take backward.
fn jump_target(i: &Instr) -> Option<u32> {
    match *i {
        Instr::Jump { target }
        | Instr::JumpIfFalse { target, .. }
        | Instr::JumpIfTrue { target, .. }
        | Instr::JumpIfNotLt { target, .. }
        | Instr::JumpIfNotLe { target, .. }
        | Instr::FuseAddIntJump { target, .. } => Some(target),
        _ => None,
    }
}

/// Whether `code` has a loop: a jump to its own position or an earlier one.
#[cfg(test)]
fn has_back_edge(code: &[Instr]) -> bool {
    code.iter()
        .enumerate()
        .any(|(ip, i)| jump_target(i).is_some_and(|t| t as usize <= ip))
}

/// Whether `code` has a loop and every instruction of every loop (from a
/// back-edge's target to the back-edge) is one the lite loop runs. Such a
/// function's loops run whole in the lite loop, calls and returns included,
/// though its prologue or epilogue holds an instruction it hands over (a
/// `MakeFunc`, a `console.log`): a function that is ALL lite instructions
/// is rare once it does anything at its edges, so a hot loop in such a
/// caller paid both loops' transitions on every call it made.
fn loops_are_lite(code: &[Instr]) -> bool {
    let mut any = false;
    for (ip, i) in code.iter().enumerate() {
        if let Some(t) = jump_target(i) {
            let t = t as usize;
            if t <= ip {
                if !code[t..=ip].iter().all(lite_instr) {
                    return false;
                }
                any = true;
            }
        }
    }
    any
}

/// Slots of the lite loop's callee cache, indexed by the callee's heap slot.
const LITE_CALL_SLOTS: usize = 256;

/// One slot of the lite loop's callee cache: the callee's bits and version
/// when `ic_call` resolved it to a plain user function the lite loop runs,
/// and that resolution, with the callee's frame facts for the push
/// (`Vm::lean_call_desc`; in the padding the slot had). What a Func or
/// Closure object calls never changes and a reused slot has a new version,
/// so it depends on the callee alone.
#[derive(Clone, Copy)]
struct LiteCall {
    bits: u64,
    ver: u32,
    fid: u32,
    closure: u32,
    desc: u32,
}

impl LiteCall {
    const EMPTY: LiteCall = LiteCall {
        bits: u64::MAX,
        ver: 0,
        fid: 0,
        closure: 0,
        desc: 0,
    };
}

/// Slots of the lite loop's method cache, indexed by the call site.
const LITE_METHOD_SLOTS: usize = 256;

/// One slot of the lite loop's method cache: a `CallMethod` site's last
/// answer by the receiver's hidden class, as the site's own cache way held
/// it (`Vm::ic_method_way`), with the callee's resolution and frame facts.
/// A hit checks that way's guards and nothing else: `ic_call_method`'s
/// site lookup, its ways' loop, the callee's kind and flags and the key's
/// text were three times the cost of the push on an object method's call.
#[derive(Clone, Copy)]
struct LiteMethod {
    /// The site: its function (`u32::MAX` for none) and position.
    site_fid: u32,
    site_ip: u32,
    callee: u64,
    /// The receiver's hidden class (never `DICT`).
    shape: u32,
    /// The receiver's own data slot holding the method, or `u32::MAX` for
    /// a method of the receiver's class.
    slot: u32,
    /// An own method: the callee's version. A class method: the class.
    ver: u32,
    /// A class method: the class's version and `class_proto_epoch`.
    class_ver: u32,
    epoch: u32,
    fid: u32,
    closure: u32,
    desc: u32,
}

impl LiteMethod {
    const EMPTY: LiteMethod = LiteMethod {
        site_fid: u32::MAX,
        site_ip: 0,
        callee: 0,
        shape: 0,
        slot: 0,
        ver: 0,
        class_ver: 0,
        epoch: 0,
        fid: 0,
        closure: 0,
        desc: 0,
    };
}

env_off_switch! {
    /// `ZIPP_NO_LITE_LEAF=1`: every call the lite loop makes takes a frame
    /// (`leaf_scan`), for A/B.
    #[inline]
    fn leaf_enabled() = "ZIPP_NO_LITE_LEAF"
}

/// A function's class as a frameless leaf (`LiteState::leaf`).
const LEAF_UNDECIDED: u8 = 0;
/// A leaf whatever its `this`.
const LEAF_ANY: u8 = 1;
/// A leaf when its caller passes its `this` (it reads it).
const LEAF_THIS: u8 = 2;
const LEAF_NO: u8 = 3;
/// Registers a leaf may have: its window is on the stack.
const LEAF_MAX_REGS: usize = 16;
/// Declines (`Vm::lite_leaf_run`) after which a leaf takes a frame for good.
const LEAF_BAIL_LIMIT: u8 = 32;

/// Whether `p` is a leaf the lite loop runs without a frame: number
/// arithmetic, comparisons, strict equality, moves, captured-variable,
/// global and own-property reads, and forward branches on them, none of
/// which can call anything, throw, allocate or write outside its
/// registers. Such a body's frame was most of its call: a method's push,
/// return and their bookkeeping ran four times its body. With no backward
/// jump it runs each instruction at most once. Not a generator, async
/// function or one that needs its `arguments`; an arrow only if it never
/// reads its `this`.
fn leaf_scan(p: &crate::bytecode::FuncProto) -> u8 {
    if p.is_generator
        || p.is_async
        || p.rest_reg.is_some()
        || p.arguments_reg.is_some()
        || p.reg_count as usize > LEAF_MAX_REGS
        || (p.param_count as usize) >= LEAF_MAX_REGS
    {
        return LEAF_NO;
    }
    let mut reads_this = false;
    for (ip, i) in p.code.iter().enumerate() {
        let forward = |t: u32| t as usize > ip && (t as usize) < p.code.len();
        let srcs: &[u16] = match *i {
            Instr::LoadInt { .. }
            | Instr::LoadBool { .. }
            | Instr::LoadNull { .. }
            | Instr::LoadUndefined { .. }
            | Instr::LoadGlobal { .. }
            | Instr::UpvalGet { .. }
            | Instr::ReturnUndefined => &[],
            Instr::LoadConst { idx, .. } if p.constants.get(idx as usize).is_some_and(|c| c.is_number()) => &[],
            Instr::Move { src, .. } | Instr::Return { src } => &[src],
            Instr::AddInt { a, .. } | Instr::Neg { a, .. } | Instr::Not { a, .. } => &[a],
            Instr::Add { a, b, .. }
            | Instr::Sub { a, b, .. }
            | Instr::Mul { a, b, .. }
            | Instr::Lt { a, b, .. }
            | Instr::Le { a, b, .. }
            | Instr::Gt { a, b, .. }
            | Instr::Ge { a, b, .. }
            | Instr::Eq { a, b, .. }
            | Instr::Ne { a, b, .. }
            | Instr::Bitwise { a, b, .. } => &[a, b],
            Instr::GetProp { obj, name, .. } if !is_private_key(&p.string_constants[name as usize]) => &[obj],
            Instr::Jump { target } if forward(target) => &[],
            Instr::JumpIfFalse { cond, target } | Instr::JumpIfTrue { cond, target } if forward(target) => &[cond],
            _ => return LEAF_NO,
        };
        reads_this |= srcs.contains(&0);
    }
    // Every path ends at a return (the compiler closes every body with one).
    if !matches!(p.code.last(), Some(Instr::Return { .. } | Instr::ReturnUndefined)) {
        return LEAF_NO;
    }
    match (reads_this, p.lexical_this) {
        (false, _) => LEAF_ANY,
        (true, false) => LEAF_THIS,
        (true, true) => LEAF_NO,
    }
}

/// The method cache slot of the site whose instruction is `at`: its address,
/// which the dispatch has in hand. A hash of the function and position was
/// computed for every instruction the loop ran, hoisted to its dispatch.
#[inline(always)]
fn lite_method_slot(at: &Instr) -> usize {
    (at as *const Instr as usize / std::mem::size_of::<Instr>()) & (LITE_METHOD_SLOTS - 1)
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
    methods: Box<[LiteMethod]>,
    /// Per function id: its class as a frameless leaf (`LEAF_*`).
    leaf: Vec<u8>,
    /// Per function id: the declines its leaf runs have had.
    leaf_bails: Vec<u8>,
}

impl Default for LiteState<'_> {
    fn default() -> Self {
        LiteState {
            ok: Vec::new(),
            code: Vec::new(),
            budget: Vec::new(),
            leaf: Vec::new(),
            leaf_bails: Vec::new(),
            calls: vec![LiteCall::EMPTY; LITE_CALL_SLOTS].into_boxed_slice(),
            methods: vec![LiteMethod::EMPTY; LITE_METHOD_SLOTS].into_boxed_slice(),
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
            Some(&LITE_SMALL) | Some(&LITE_FULL) => true,
            Some(&LITE_NO) => false,
            _ => self.lite_decide(fid),
        }
    }

    /// Decide function `fid` again at its next frame: its code has changed
    /// (a deferred body replaced its stub, `engine::lazy_install`).
    pub(crate) fn lite_forget(&mut self, fid: u32) {
        if let Some(ok) = self.lite.ok.get_mut(fid as usize) {
            *ok = LITE_UNDECIDED;
        }
        if let Some(l) = self.lite.leaf.get_mut(fid as usize) {
            *l = LEAF_UNDECIDED;
        }
    }

    /// Run `fid`'s body without a frame when it is a leaf (`leaf_scan`):
    /// its result, or `None` when it is not one or this run declined
    /// (`leaf_run`), and the call takes its frame. `this_ok`: the caller
    /// passes the callee's `this` (a method call's receiver), which a leaf
    /// that reads its `this` needs.
    ///
    /// Inline, only the test of the callee's class: a call into the loop's
    /// function inline grew its every arm's register shuffles.
    #[allow(clippy::too_many_arguments)]
    #[inline(always)]
    fn lite_leaf(
        &mut self,
        fid: u32,
        closure: u32,
        this: Value,
        this_ok: bool,
        base: usize,
        arg_base: u16,
        argc: u16,
    ) -> Option<Value> {
        if self.lite.leaf.get(fid as usize) == Some(&LEAF_NO) {
            return None;
        }
        self.lite_leaf_try(fid, closure, this, this_ok, base, arg_base, argc)
    }

    /// [`Self::lite_leaf`] for the main loop's calls an interpreter-only VM
    /// makes (its frames' calls the lite loop does not run: a script's top
    /// level past its first hand-over, a `super.m()`).
    #[allow(clippy::too_many_arguments)]
    #[inline(always)]
    pub(crate) fn leaf_call(
        &mut self,
        fid: u32,
        closure: u32,
        this: Value,
        this_ok: bool,
        base: usize,
        arg_base: u16,
        argc: u16,
    ) -> Option<Value> {
        self.lite_leaf(fid, closure, this, this_ok, base, arg_base, argc)
    }

    /// [`Self::lite_leaf`] for a callee that may be a leaf.
    #[allow(clippy::too_many_arguments)]
    #[inline(never)]
    fn lite_leaf_try(
        &mut self,
        fid: u32,
        closure: u32,
        this: Value,
        this_ok: bool,
        base: usize,
        arg_base: u16,
        argc: u16,
    ) -> Option<Value> {
        let state = match self.lite.leaf.get(fid as usize) {
            Some(&s) if s != LEAF_UNDECIDED => s,
            _ => self.lite_leaf_decide(fid),
        };
        if state == LEAF_NO || (state == LEAF_THIS && !this_ok) || self.frames.len() >= MAX_FRAMES {
            return None;
        }
        self.lite_leaf_run(fid, closure, this, base, arg_base, argc)
    }

    #[cold]
    #[inline(never)]
    fn lite_leaf_decide(&mut self, fid: u32) -> u8 {
        let i = fid as usize;
        if self.lite.leaf.len() <= i {
            self.lite.leaf.resize(i + 1, LEAF_UNDECIDED);
            self.lite.leaf_bails.resize(i + 1, 0);
        }
        let s = if leaf_enabled() {
            leaf_scan(self.func(i))
        } else {
            LEAF_NO
        };
        self.lite.leaf[i] = s;
        s
    }

    /// One run of leaf `fid` over a register window on the stack: `this`,
    /// the arguments from the caller's `base + arg_base`, the rest
    /// `undefined`. Its ops are the interpreter's fast paths exactly, and
    /// any operand they do not take (a non-number, a property the site's
    /// own-data way does not answer) declines before the body has done
    /// anything a caller could see, so the call then runs it again in a
    /// frame. A leaf that declines `LEAF_BAIL_LIMIT` times stays a frame
    /// call.
    #[allow(clippy::too_many_arguments)]
    #[inline(always)]
    fn lite_leaf_run(
        &mut self,
        fid: u32,
        closure: u32,
        this: Value,
        base: usize,
        arg_base: u16,
        argc: u16,
    ) -> Option<Value> {
        use crate::bytecode::BitwiseOp as B;
        let p = self.func(fid as usize);
        let code: &'p [Instr] = &p.code;
        let mut r = [Value::UNDEFINED; LEAF_MAX_REGS];
        r[0] = this;
        let n = (argc as usize).min(p.param_count as usize).min(LEAF_MAX_REGS - 1);
        for (k, slot) in r[1..=n].iter_mut().enumerate() {
            *slot = self.get(base, arg_base + k as u16);
        }
        let mut ip = 0;
        macro_rules! decline {
            () => {{
                let b = &mut self.lite.leaf_bails[fid as usize];
                *b += 1;
                if *b >= LEAF_BAIL_LIMIT {
                    self.lite.leaf[fid as usize] = LEAF_NO;
                }
                return None;
            }};
        }
        macro_rules! num2 {
            ($a:expr, $b:expr, $int:expr, $f:expr) => {{
                let (x, y) = (r[$a as usize], r[$b as usize]);
                if x.is_int() && y.is_int() {
                    $int(x.as_int(), y.as_int())
                } else if x.is_number() && y.is_number() {
                    $f(x.as_f64(), y.as_f64())
                } else {
                    decline!();
                }
            }};
        }
        loop {
            match code[ip] {
                Instr::LoadInt { dst, val } => r[dst as usize] = Value::int(val),
                Instr::LoadConst { dst, idx } => r[dst as usize] = p.constants[idx as usize],
                Instr::LoadBool { dst, val } => r[dst as usize] = Value::bool(val),
                Instr::LoadNull { dst } => r[dst as usize] = Value::NULL,
                Instr::LoadUndefined { dst } => r[dst as usize] = Value::UNDEFINED,
                Instr::Move { dst, src } => r[dst as usize] = r[src as usize],
                // The lite loop's reads: a binding in its TDZ, and a global
                // the slow route answers, throw or run code there.
                Instr::LoadGlobal { dst, idx } => {
                    let v = self.globals[idx as usize];
                    if v.is_uninitialized() || (self.global_route_epoch != 0 && self.global_real_own_route(idx)) {
                        decline!();
                    }
                    r[dst as usize] = v;
                }
                Instr::UpvalGet { dst, idx } => {
                    if closure == NO_CLOSURE {
                        decline!();
                    }
                    let v = self.heap.cell_get(self.closure_upvalue(closure, idx));
                    if v.is_uninitialized() {
                        decline!();
                    }
                    r[dst as usize] = v;
                }
                Instr::Eq { dst, a, b } | Instr::Ne { dst, a, b } => {
                    let (x, y) = (r[a as usize], r[b as usize]);
                    // Two different heap values may be equal strings, which
                    // a comparison may flatten: the frame's.
                    if x.is_heap() && y.is_heap() && x.bits() != y.bits() {
                        decline!();
                    }
                    let eq = self.strict_eq_fast(x, y);
                    r[dst as usize] = Value::bool(eq == matches!(code[ip], Instr::Eq { .. }));
                }
                Instr::Not { dst, a } => match r[a as usize].truthy_primitive() {
                    Some(t) => r[dst as usize] = Value::bool(!t),
                    None => decline!(),
                },
                Instr::Neg { dst, a } => {
                    let x = r[a as usize];
                    r[dst as usize] = if x.is_int() {
                        match x.as_int() {
                            0 => Value::num(-0.0),
                            i => match i.checked_neg() {
                                Some(v) => Value::int(v),
                                None => Value::num(-(i as f64)),
                            },
                        }
                    } else if x.is_double() {
                        Value::num(-x.as_f64())
                    } else {
                        decline!();
                    };
                }
                Instr::Jump { target } => {
                    ip = target as usize;
                    continue;
                }
                Instr::JumpIfFalse { cond, target } | Instr::JumpIfTrue { cond, target } => {
                    let Some(t) = r[cond as usize].truthy_primitive() else {
                        decline!();
                    };
                    if t == matches!(code[ip], Instr::JumpIfTrue { .. }) {
                        ip = target as usize;
                        continue;
                    }
                }
                Instr::Add { dst, a, b } => {
                    r[dst as usize] = num2!(
                        a,
                        b,
                        |x: i32, y: i32| match x.checked_add(y) {
                            Some(v) => Value::int(v),
                            None => Value::num(x as f64 + y as f64),
                        },
                        |x: f64, y: f64| Value::num(x + y)
                    )
                }
                Instr::Sub { dst, a, b } => {
                    r[dst as usize] = num2!(
                        a,
                        b,
                        |x: i32, y: i32| match x.checked_sub(y) {
                            Some(v) => Value::int(v),
                            None => Value::num(x as f64 - y as f64),
                        },
                        |x: f64, y: f64| Value::num(x - y)
                    )
                }
                Instr::Mul { dst, a, b } => {
                    r[dst as usize] = num2!(
                        a,
                        b,
                        // A zero product with a negative operand is -0.
                        |x: i32, y: i32| match x.checked_mul(y) {
                            Some(v) if v != 0 || (x | y) >= 0 => Value::int(v),
                            _ => Value::num(x as f64 * y as f64),
                        },
                        |x: f64, y: f64| Value::num(x * y)
                    )
                }
                Instr::Lt { dst, a, b } => r[dst as usize] = num2!(a, b, |x, y| Value::bool(x < y), |x, y| Value::bool(x < y)),
                Instr::Le { dst, a, b } => r[dst as usize] = num2!(a, b, |x, y| Value::bool(x <= y), |x, y| Value::bool(x <= y)),
                Instr::Gt { dst, a, b } => r[dst as usize] = num2!(a, b, |x, y| Value::bool(x > y), |x, y| Value::bool(x > y)),
                Instr::Ge { dst, a, b } => r[dst as usize] = num2!(a, b, |x, y| Value::bool(x >= y), |x, y| Value::bool(x >= y)),
                Instr::AddInt { dst, a, imm, .. } => {
                    let x = r[a as usize];
                    r[dst as usize] = if x.is_int() {
                        match x.as_int().checked_add(imm) {
                            Some(v) => Value::int(v),
                            None => Value::num(x.as_int() as f64 + imm as f64),
                        }
                    } else if x.is_number() {
                        Value::num(x.as_f64() + imm as f64)
                    } else {
                        decline!();
                    };
                }
                Instr::Bitwise { dst, a, b, op } => {
                    // `fused_bitwise`'s Number path: ToInt32 of each.
                    let (x, y) = (r[a as usize], r[b as usize]);
                    let (x, y) = if x.is_int() && y.is_int() {
                        (x.as_int(), y.as_int())
                    } else {
                        match super::dispatch::number_int32(x).zip(super::dispatch::number_int32(y)) {
                            Some(p) => p,
                            None => decline!(),
                        }
                    };
                    r[dst as usize] = match op {
                        B::And => Value::int(x & y),
                        B::Or => Value::int(x | y),
                        B::Xor => Value::int(x ^ y),
                        B::Shl => Value::int(x.wrapping_shl(y as u32 & 31)),
                        B::Shr => Value::int(x >> (y as u32 & 31)),
                        B::Ushr => Value::num(((x as u32) >> (y as u32 & 31)) as f64),
                    };
                }
                Instr::GetProp { dst, obj, .. } => match self.ic_get_prop_own_hit(fid, ip, r[obj as usize]) {
                    Some(v) => r[dst as usize] = v,
                    None => decline!(),
                },
                Instr::Return { src } => return Some(r[src as usize]),
                Instr::ReturnUndefined => return Some(Value::UNDEFINED),
                _ => decline!(),
            }
            ip += 1;
        }
    }

    /// Whether `fid` (which the lite loop runs) runs in the full copy.
    #[inline(always)]
    fn lite_is_full(&self, fid: u32) -> bool {
        self.lite.ok[fid as usize] == LITE_FULL
    }

    /// Every instruction of `fid`, in its code and its superinstruction copy,
    /// is one the lite loop runs, and its frames need none of `setup_call`'s
    /// rarer work.
    #[cold]
    #[inline(never)]
    fn lite_decide(&mut self, fid: u32) -> bool {
        let i = fid as usize;
        let p = self.func(i);
        // Every instruction the lite loop's, or (`loops_are_lite`) every
        // instruction of its loops, the rest handed over where they stand.
        let all = p.code.iter().all(lite_instr) && Self::lite_code(p).iter().all(lite_instr);
        // An async function's activation is an ordinary frame between its
        // awaits (`drive_async` pushes one per resumption and runs it here);
        // its `Await` is handed over, and its return pops the frame its driver
        // stopped at, as a call's does. A module's top level is one, so its
        // loops ran in the main loop always. A generator's frame is not: its
        // registers move in and out of the heap object at every resume.
        let ok = lite_enabled()
            && !p.is_generator
            && (!p.is_async || lite_async_enabled())
            // An arrow's frame takes its captured `this` (`setup_call`; the
            // lean push hands it there), and its body is ordinary code.
            && p.rest_reg.is_none()
            && p.arguments_reg.is_none()
            && (all || (partial_lite_enabled() && loops_are_lite(&p.code) && loops_are_lite(Self::lite_code(p))))
            // A private member's read or write is the main arm's before any
            // cache (brand checks, the private side table).
            && !p.code.iter().any(|i| match *i {
                Instr::GetProp { name, .. } | Instr::SetProp { name, .. } => {
                    is_private_key(&p.string_constants[name as usize])
                }
                _ => false,
            });
        let small = ok && all && p.code.iter().all(lite_small_instr) && Self::lite_code(p).iter().all(lite_small_instr);
        if self.lite.ok.len() <= i {
            self.lite.ok.resize(i + 1, LITE_UNDECIDED);
            self.lite.code.resize(i + 1, &[]);
            self.lite.budget.resize(i + 1, 0);
        }
        self.lite.ok[i] = match (ok, small) {
            (false, _) => LITE_NO,
            (true, true) => LITE_SMALL,
            (true, false) => LITE_FULL,
        };
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
            self.lite.ok[fid as usize] = LITE_NO;
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
    /// `ip`, with its frame descriptor (`Vm::lean_call_desc`): the lite
    /// cache's answer, else `ic_call`'s (remembered when the lite loop runs
    /// the callee).
    #[inline(always)]
    fn lite_resolve(&mut self, func_id: u32, ip: usize, callee: Value) -> Option<(u32, u32, u32)> {
        if callee.is_heap() {
            let idx = callee.heap_index();
            let e = self.lite.calls[idx as usize & (LITE_CALL_SLOTS - 1)];
            if e.bits == callee.bits() && e.ver == self.heap.version_of(idx) {
                return Some((e.fid, e.closure, e.desc));
            }
        }
        let call = self.ic_call(func_id, ip, callee)?;
        let desc = self.lean_call_desc(call.fid);
        if self.lite_eligible(call.fid) {
            let idx = callee.heap_index();
            self.lite.calls[idx as usize & (LITE_CALL_SLOTS - 1)] = LiteCall {
                bits: callee.bits(),
                ver: self.heap.version_of(idx),
                fid: call.fid,
                closure: call.closure,
                desc,
            };
        }
        Some((call.fid, call.closure, desc))
    }

    /// The method `recv.<name>` resolves to for the `CallMethod` at `ip` in
    /// `func_id`, an ordinary object or class instance its receiver: the
    /// lite method cache's answer, else `ic_call_method`'s (remembered by
    /// the way that holds it), with the callee's frame descriptor. `None`
    /// for anything the method cache does not answer (a private name, a
    /// native, an accessor).
    #[inline(always)]
    fn lite_method(&mut self, at: &Instr, func_id: u32, ip: usize, recv: Value, name: u32) -> Option<(u32, u32, Value, u32)> {
        let slot = lite_method_slot(at);
        let e = &self.lite.methods[slot];
        if e.site_fid == func_id && e.site_ip == ip as u32 {
            if let Some((_, m)) = self.ic_recv_map(recv) {
                if m.shape() == e.shape
                    && if e.slot != u32::MAX {
                        m.val_at(e.slot as usize).bits() == e.callee
                            && self.heap.version_of(Value::from_bits(e.callee).heap_index()) == e.ver
                    } else {
                        m.class == Some(e.ver)
                            && self.heap.version_of(e.ver) == e.class_ver
                            && self.class_proto_epoch == e.epoch
                    }
                {
                    return Some((e.fid, e.closure, Value::from_bits(e.callee), e.desc));
                }
            }
        }
        self.lite_method_miss(slot, func_id, ip, recv, name)
    }

    /// The `CallMethod` at `at` (`ip` in `func_id`) on an ordinary object
    /// or class instance `recv`: the method cache's callee run as a leaf
    /// or its frame pushed. Out of line: inline, the cache, the leaf test
    /// and the push grew the loop's register shuffles in every arm, and a
    /// loop with no call ran 11% more cycles.
    #[allow(clippy::too_many_arguments)]
    #[inline(never)]
    fn lite_call_method(
        &mut self,
        at: &Instr,
        func_id: u32,
        ip: usize,
        recv: Value,
        name: u32,
        base: usize,
        arg_base: u16,
        argc: u16,
        dst: u16,
    ) -> Result<LiteCallOut, Thrown> {
        let Some((fid, closure, callee, desc)) = self.lite_method(at, func_id, ip, recv, name) else {
            return Ok(LiteCallOut::HandOver);
        };
        if let Some(v) = self.lite_leaf(fid, closure, recv, true, base, arg_base, argc) {
            return Ok(LiteCallOut::Value(v));
        }
        self.setup_call_method_desc(fid, closure, desc, recv, base, arg_base, argc, dst, ip + 1, callee)?;
        Ok(LiteCallOut::Pushed(fid))
    }

    /// [`Self::lite_method`] past its cache, which it fills at `slot`.
    #[inline(never)]
    fn lite_method_miss(
        &mut self,
        slot: usize,
        func_id: u32,
        ip: usize,
        recv: Value,
        name: u32,
    ) -> Option<(u32, u32, Value, u32)> {
        let key: &'p str = self.func(func_id as usize).string_constants[name as usize].as_str();
        if is_private_key(key) {
            return None;
        }
        let (fid, closure, callee) = self.ic_call_method(func_id, ip, recv, key)?;
        let desc = self.lean_call_desc(fid);
        if let Some((shape, own, class, class_ver, epoch)) = self.ic_method_way(func_id, ip, recv, callee) {
            let ver = if own == u32::MAX {
                class
            } else {
                self.heap.version_of(callee.heap_index())
            };
            self.lite.methods[slot] = LiteMethod {
                site_fid: func_id,
                site_ip: ip as u32,
                callee: callee.bits(),
                shape,
                slot: own,
                ver,
                class_ver,
                epoch,
                fid,
                closure,
                desc,
            };
        }
        Some((fid, closure, callee, desc))
    }

    /// Run the top frame, and the eligible frames it calls or returns to, in
    /// the lite loop (see the module doc): in the small copy while the
    /// functions are small-class, then in the full copy, which runs both
    /// classes, from the first full-class function on.
    #[inline(never)]
    pub(crate) fn run_lite(&mut self, stop_depth: usize) -> Result<LiteExit, Thrown> {
        // The entered function and the instructions this entry has run
        // (`LITE_MIN_ENTRY_STEPS`), across both copies.
        let entry_fid = self.frames[self.frames.len() - 1].func;
        let mut steps: u64 = 0;
        if !self.lite_is_full(entry_fid) {
            match self.run_lite_in::<false>(stop_depth, entry_fid, &mut steps)? {
                LiteStep::Exit(e) => return Ok(e),
                LiteStep::Full => {}
            }
        }
        match self.run_lite_in::<true>(stop_depth, entry_fid, &mut steps)? {
            LiteStep::Exit(e) => Ok(e),
            // The full copy runs every class; it never asks for itself.
            LiteStep::Full => unreachable!("the full lite copy switched to itself"),
        }
    }

    /// One copy of the lite loop: `FULL` runs every lite instruction, the
    /// small copy only `lite_small_instr`'s and returns `LiteStep::Full` at a
    /// call into or a return to a full-class function.
    #[inline(never)]
    fn run_lite_in<const FULL: bool>(
        &mut self,
        stop_depth: usize,
        entry_fid: u32,
        steps_total: &mut u64,
    ) -> Result<LiteStep, Thrown> {
        let mut top = self.frames.len() - 1;
        let mut func_id = self.frames[top].func;
        let mut base = self.frames[top].base;
        let mut ip = self.frames[top].ip;
        let mut code: &'p [Instr] = self.lite_fn_code(func_id);
        // The top frame's upvalue list (`closure_upvalue_cached`): taken on
        // its first upvalue access, reset at every frame change.
        #[cfg(not(feature = "safe-sandbox"))]
        let mut upv: (*const u32, usize) = (std::ptr::null(), 0);
        let mut steps: u64 = *steps_total;
        macro_rules! exit {
            ($e:expr) => {{
                *steps_total = steps;
                return Ok($e);
            }};
        }
        macro_rules! short_entry {
            () => {
                if steps < LITE_MIN_ENTRY_STEPS {
                    self.lite_handed_over(entry_fid);
                }
            };
        }
        // Hand the instruction at `ip` to the main loop.
        macro_rules! hand_over {
            () => {{
                self.frames[top].ip = ip;
                self.lite_handed_over(func_id);
                exit!(LiteStep::Exit(LiteExit::Resume));
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
                    short_entry!();
                    exit!(LiteStep::Exit(LiteExit::Done(v)));
                }
                self.maybe_gc();
                top = self.frames.len() - 1;
                func_id = self.frames[top].func;
                if !self.lite_eligible(func_id) {
                    short_entry!();
                    exit!(LiteStep::Exit(LiteExit::Resume));
                }
                if !FULL && self.lite_is_full(func_id) {
                    exit!(LiteStep::Full);
                }
                base = self.frames[top].base;
                ip = self.frames[top].ip;
                code = self.lite_fn_code(func_id);
                #[cfg(not(feature = "safe-sandbox"))]
                {
                    upv = (std::ptr::null(), 0);
                }
            }};
        }
        // Run the frame a call just pushed for `fid` here, or hand it (and
        // the caller after it, to its next frame transition) to the main
        // loop: the `Call` arm's steps after its push.
        macro_rules! lite_enter {
            ($fid:expr) => {{
                let fid = $fid;
                steps += LITE_CALL_STEPS;
                // The main loop's frame-transition safe point.
                self.maybe_gc();
                if !self.lite_eligible(fid) {
                    self.lite_handed_over(func_id);
                    exit!(LiteStep::Exit(LiteExit::Resume));
                }
                if !FULL && self.lite_is_full(fid) {
                    exit!(LiteStep::Full);
                }
                top = self.frames.len() - 1;
                func_id = fid;
                base = self.frames[top].base;
                ip = 0;
                code = self.lite_fn_code(func_id);
                #[cfg(not(feature = "safe-sandbox"))]
                {
                    upv = (std::ptr::null(), 0);
                }
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
                Instr::FuseMove2 { dst, src, dst2, src2 } => {
                    let v = self.get(base, src);
                    self.set(base, dst, v);
                    let v = self.get(base, src2);
                    self.set(base, dst2, v);
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
                    let r = self.add_int_value(va, imm.into(), upd)?;
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
                        steps += LITE_LOOP_STEPS;
                        self.maybe_gc();
                    }
                    ip = t;
                }
                Instr::FuseAddIntJump { dst, a, imm, upd, target } if FULL => {
                    let va = self.get(base, a);
                    let r = self.add_int_value(va, imm, upd)?;
                    self.set(base, dst, r);
                    // The back-edge `Jump` at `ip + 1`: its GC safe point.
                    steps += LITE_LOOP_STEPS;
                    self.maybe_gc();
                    ip = target as usize;
                }
                Instr::JumpIfFalse { cond, target } if FULL => {
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
                Instr::JumpIfTrue { cond, target } if FULL => {
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
                Instr::Sub { dst, a, b } if FULL => {
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
                Instr::Mul { dst, a, b } if FULL => {
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
                Instr::Bitwise { dst, a, b, op } if FULL => {
                    let (va, vb) = (self.get(base, a), self.get(base, b));
                    let r = self.fused_bitwise(va, vb, op)?;
                    self.set(base, dst, r);
                    ip += 1;
                }
                Instr::FuseLoadIntBitwise { ldst, val, dst, a, b, op } if FULL => {
                    self.set(base, ldst, Value::int(val));
                    let (va, vb) = (self.get(base, a), self.get(base, b));
                    let r = self.fused_bitwise(va, vb, op)?;
                    self.set(base, dst, r);
                    ip += 2;
                }
                Instr::Eq { dst, a, b } if FULL => {
                    let (va, vb) = (self.get(base, a), self.get(base, b));
                    let r = self.strict_eq_fast(va, vb);
                    self.set(base, dst, Value::bool(r));
                    ip += 1;
                }
                Instr::Ne { dst, a, b } if FULL => {
                    let (va, vb) = (self.get(base, a), self.get(base, b));
                    let r = self.strict_eq_fast(va, vb);
                    self.set(base, dst, Value::bool(!r));
                    ip += 1;
                }
                Instr::FuseEqJump { dst, a, b, ne, when, target } if FULL => {
                    let (va, vb) = (self.get(base, a), self.get(base, b));
                    let eq = self.strict_eq_fast(va, vb);
                    let r = eq != ne;
                    self.set(base, dst, Value::bool(r));
                    // Forward only (see the variant).
                    ip = if r == when { target as usize } else { ip + 2 };
                }
                Instr::FuseIntEqJump { ldst, val, a, dst, target } if FULL => {
                    self.set(base, ldst, Value::int(val));
                    let va = self.get(base, a);
                    // As the main arm: an Int or a Number of that value.
                    let r = if va.is_int() {
                        va.as_int() == val
                    } else {
                        va.is_double() && va.as_f64() == val as f64
                    };
                    self.set(base, dst, Value::bool(r));
                    ip = if r { target as usize } else { ip + 3 };
                }
                Instr::FuseIntSwitch { ldst, a, dst, n } if FULL => {
                    let running = self.func(func_id as usize);
                    ip = self.int_switch(running, base, ip, ldst, a, dst, n);
                }
                // A global's store where the main arms' is the slot write: an
                // initialized slot no defineProperty'd own property governs,
                // and no `$262.evalScript` const anywhere.
                Instr::StoreGlobal { idx, src } | Instr::StoreGlobalStrict { idx, src } if FULL => {
                    if self.globals[idx as usize].is_uninitialized()
                        || !self.eval_const_globals.is_empty()
                        || (self.global_route_epoch != 0 && self.global_real_own_route(idx))
                    {
                        hand_over!();
                    }
                    self.globals[idx as usize] = self.get(base, src);
                    ip += 1;
                }
                Instr::FuseIntNeJump { ldst, val, a, dst, target } if FULL => {
                    self.set(base, ldst, Value::int(val));
                    let va = self.get(base, a);
                    let r = if va.is_int() {
                        va.as_int() == val
                    } else {
                        va.is_double() && va.as_f64() == val as f64
                    };
                    self.set(base, dst, Value::bool(r));
                    ip = if r { ip + 3 } else { target as usize };
                }
                Instr::GetIndex { dst, obj, key } if FULL => {
                    let (o, k) = (self.get(base, obj), self.get(base, key));
                    if !o.is_heap() {
                        hand_over!();
                    }
                    // The main arm's two leaves from one look at the object:
                    // a numeric typed array's element, then (as
                    // `interp_dense_int_get`) a present dense element of an
                    // array with no element override and no arguments map.
                    let idx = o.heap_index();
                    let r = match self.heap.get(idx) {
                        HeapObj::TypedArray {
                            buffer,
                            kind,
                            byte_offset,
                            length,
                        } => {
                            if native::TA_KINDS[*kind as usize].2 {
                                hand_over!();
                            }
                            let Some(i) = array_index(k) else {
                                hand_over!();
                            };
                            let view = crate::vm::typedarray::TaInterpNumericView::from_matched_non_bigint(
                                idx,
                                *buffer,
                                *kind,
                                *byte_offset,
                                *length,
                            );
                            let Some(r) = self.ta_interp_numeric_get_view_call(view, i) else {
                                hand_over!();
                            };
                            r
                        }
                        HeapObj::Array(items) => {
                            if !k.is_int() || k.as_int() < 0 {
                                hand_over!();
                            }
                            let Some(&v) = items.get(k.as_int() as usize) else {
                                hand_over!();
                            };
                            if v.is_hole()
                                || self.arr_props.get(&idx).is_some_and(|m| m.has_element_key())
                                || (!self.arguments_objs.is_empty() && self.arguments_objs.contains_key(&idx))
                            {
                                hand_over!();
                            }
                            v
                        }
                        _ => hand_over!(),
                    };
                    self.set(base, dst, r);
                    ip += 1;
                }
                Instr::GetProp { dst, obj, name } if FULL => {
                    let o = self.get(base, obj);
                    if let Some(v) = self.ic_get_prop_own_hit(func_id, ip, o) {
                        self.set(base, dst, v);
                        ip += 1;
                        continue;
                    }
                    // The caches answer only plain objects, for which the
                    // main arm's steps before its cache answer nothing, and
                    // a function here names no private member
                    // (`lite_decide`). For any other receiver those steps
                    // are the answer: a string's, an array's or a typed
                    // array's `length`, a shapeless receiver's prototype
                    // intrinsic (`arr.push`). Anything else, and anything
                    // the cache does not answer with a value, is the main
                    // loop's.
                    if !o.is_heap() {
                        hand_over!();
                    }
                    let key: &'p str = self.func(func_id as usize).string_constants[name as usize].as_str();
                    #[cfg(not(feature = "safe-sandbox"))]
                    if let Some(v) = self.ic_get_prop_proto_hit(func_id, ip, o, key) {
                        self.ic_proto_hit_settle(o);
                        self.set(base, dst, v);
                        ip += 1;
                        continue;
                    }
                    if !matches!(self.heap.get(o.heap_index()), HeapObj::Object(_)) {
                        if key == "length" {
                            if let Some(v) = self.length_fast(o) {
                                self.set(base, dst, v);
                                ip += 1;
                                continue;
                            }
                        }
                        if let Some(bits) = self.proto_intrinsic_read(o, key) {
                            self.set(base, dst, Value::from_bits(bits));
                            ip += 1;
                            continue;
                        }
                        hand_over!();
                    }
                    match self.ic_get_prop(func_id, ip, o, key) {
                        GetAct::Value(v) => {
                            self.set(base, dst, v);
                            ip += 1;
                        }
                        _ => hand_over!(),
                    }
                }
                Instr::LoadUndefined { dst } if FULL => {
                    self.set(base, dst, Value::UNDEFINED);
                    ip += 1;
                }
                Instr::UpvalGet { dst, idx } if FULL => {
                    #[cfg(not(feature = "safe-sandbox"))]
                    let cell = self.closure_upvalue_cached(&mut upv, self.frames[top].closure, idx);
                    #[cfg(feature = "safe-sandbox")]
                    let cell = self.closure_upvalue(self.frames[top].closure, idx);
                    let v = self.heap.cell_get(cell);
                    // A binding still in its TDZ throws, in the main arm.
                    if v.is_uninitialized() {
                        hand_over!();
                    }
                    self.set(base, dst, v);
                    ip += 1;
                }
                Instr::FuseUpvalGetMove { dst, idx, mdst, msrc } if FULL => {
                    #[cfg(not(feature = "safe-sandbox"))]
                    let cell = self.closure_upvalue_cached(&mut upv, self.frames[top].closure, idx);
                    #[cfg(feature = "safe-sandbox")]
                    let cell = self.closure_upvalue(self.frames[top].closure, idx);
                    let v = self.heap.cell_get(cell);
                    if v.is_uninitialized() {
                        hand_over!();
                    }
                    self.set(base, dst, v);
                    let m = self.get(base, msrc);
                    self.set(base, mdst, m);
                    ip += 2;
                }
                Instr::UpvalSet { idx, src } if FULL => {
                    #[cfg(not(feature = "safe-sandbox"))]
                    let cell = self.closure_upvalue_cached(&mut upv, self.frames[top].closure, idx);
                    #[cfg(feature = "safe-sandbox")]
                    let cell = self.closure_upvalue(self.frames[top].closure, idx);
                    // A captured `const`, a named function expression's own
                    // name and a binding in its TDZ are the main arm's (it
                    // throws or, for the name in sloppy code, does nothing).
                    if (!self.const_cells.is_empty() && self.const_cells.contains(&cell))
                        || (!self.fn_name_cells.is_empty() && self.fn_name_cells.contains(&cell))
                        || self.heap.cell_get(cell).is_uninitialized()
                    {
                        hand_over!();
                    }
                    let v = self.get(base, src);
                    self.heap.upval_cell_set(cell, v);
                    ip += 1;
                }
                Instr::MathOp {
                    dst,
                    op,
                    callee,
                    this_v,
                    arg_base,
                    argc,
                } if FULL => {
                    // The main arm's intrinsic checks; a `Math` method that is
                    // no longer the builtin is its ordinary call, there.
                    let intrinsic = if callee == crate::bytecode::NO_REG {
                        self.math_bare_is_intrinsic(op, this_v as u32)
                    } else {
                        let (c, t) = (self.get(base, callee), self.get(base, this_v));
                        self.math_method_is_intrinsic(native::MATH_METHOD_BASE + op as u16, c, t)
                    };
                    if !intrinsic {
                        hand_over!();
                    }
                    let r = self.eval_math_value(op, base, arg_base, argc)?;
                    self.set(base, dst, r);
                    ip += 1;
                }
                Instr::SetProp { obj, val, name, .. } if FULL => {
                    let o = self.get(base, obj);
                    let v = self.get(base, val);
                    // The cache's own-data hit, the main arm's first step for
                    // a name that is not private (`lite_decide`).
                    if !self.ic_set_prop_own_hit(func_id, ip, o, v) {
                        // An ordinary object's other cached writes (a
                        // constructor's adds): the main arm's cache call, after
                        // the lanes that answer only other receivers. Anything
                        // it does not complete (a setter to call, a miss) left
                        // nothing written, and the main arm runs it again.
                        if !o.is_heap() || !matches!(self.heap.get(o.heap_index()), HeapObj::Object(_)) {
                            hand_over!();
                        }
                        let key: &'p str = self.func(func_id as usize).string_constants[name as usize].as_str();
                        if !matches!(self.ic_set_prop(func_id, ip, o, key, v), SetAct::Done) {
                            hand_over!();
                        }
                    }
                    ip += 1;
                }
                Instr::StoreGlobalResolved { idx, src } if FULL => {
                    // The main arm's slot write, for a binding its preamble
                    // (an unresolvable strict reference, a vanished or
                    // shadowed binding, a constant) does not concern.
                    if !self.strict_unresolvable_globals.is_empty()
                        || self.globals[idx as usize].is_uninitialized()
                        || !self.eval_const_globals.is_empty()
                        || (self.global_route_epoch != 0 && self.global_real_own_route(idx))
                    {
                        hand_over!();
                    }
                    self.globals[idx as usize] = self.get(base, src);
                    ip += 1;
                }
                Instr::LoadConst { dst, idx } if FULL => {
                    let v = self.resolve_const_slot(func_id, idx as u32);
                    self.set(base, dst, v);
                    ip += 1;
                }
                Instr::LoadNull { dst } if FULL => {
                    self.set(base, dst, Value::NULL);
                    ip += 1;
                }
                Instr::LoadBool { dst, val } if FULL => {
                    self.set(base, dst, Value::bool(val));
                    ip += 1;
                }
                Instr::Mod { dst, a, b } if FULL => {
                    // The main arm's number cases; anything else is its
                    // ToNumeric path.
                    let (va, vb) = (self.get(base, a), self.get(base, b));
                    let r = if va.is_int() && vb.is_int() {
                        let ia = va.as_int();
                        match ia.checked_rem(vb.as_int()) {
                            Some(rem) if rem != 0 || ia >= 0 => Value::int(rem),
                            _ => Value::num(va.as_f64() % vb.as_f64()),
                        }
                    } else if va.is_number() && vb.is_number() {
                        Value::num(va.as_f64() % vb.as_f64())
                    } else {
                        hand_over!();
                    };
                    self.set(base, dst, r);
                    ip += 1;
                }
                Instr::Neg { dst, a } if FULL => {
                    let va = self.get(base, a);
                    let r = if va.is_int() {
                        let i = va.as_int();
                        if i == 0 {
                            Value::num(-0.0)
                        } else {
                            match i.checked_neg() {
                                Some(v) => Value::int(v),
                                None => Value::num(-(i as f64)),
                            }
                        }
                    } else if va.is_double() {
                        Value::num(-va.as_f64())
                    } else {
                        hand_over!();
                    };
                    self.set(base, dst, r);
                    ip += 1;
                }
                Instr::Not { dst, a } if FULL => {
                    let va = self.get(base, a);
                    let t = self.truthy(va);
                    self.set(base, dst, Value::bool(!t));
                    ip += 1;
                }
                Instr::Lt { dst, a, b } if FULL => {
                    let r = self.cmp_lt(base, a, b, true)?;
                    self.set(base, dst, Value::bool(r));
                    ip += 1;
                }
                Instr::Le { dst, a, b } if FULL => {
                    let r = self.cmp_le(base, a, b, true)?;
                    self.set(base, dst, Value::bool(r));
                    ip += 1;
                }
                Instr::Gt { dst, a, b } if FULL => {
                    let r = self.cmp_lt(base, b, a, false)?;
                    self.set(base, dst, Value::bool(r));
                    ip += 1;
                }
                Instr::Ge { dst, a, b } if FULL => {
                    let r = self.cmp_le(base, b, a, false)?;
                    self.set(base, dst, Value::bool(r));
                    ip += 1;
                }
                Instr::CellGet { dst, cell } if FULL => {
                    let v = self.heap.cell_get(self.get(base, cell).heap_index());
                    // A binding in its TDZ throws, in the main arm.
                    if v.is_uninitialized() {
                        hand_over!();
                    }
                    self.set(base, dst, v);
                    ip += 1;
                }
                Instr::CellSet { cell, src } if FULL => {
                    let cell_idx = self.get(base, cell).heap_index();
                    let v = self.get(base, src);
                    self.heap.cell_set(cell_idx, v);
                    ip += 1;
                }
                Instr::SetIndex { obj, key, val } if FULL => {
                    // The main arm's numeric typed-array store first, then its
                    // general store (which runs no frame of this loop).
                    let (o, k, v) = (self.get(base, obj), self.get(base, key), self.get(base, val));
                    if o.is_heap() && v.is_number() {
                        let ta_idx = o.heap_index();
                        if let HeapObj::TypedArray {
                            buffer,
                            kind,
                            byte_offset,
                            length,
                        } = self.heap.get(ta_idx)
                        {
                            if !native::TA_KINDS[*kind as usize].2
                                && (self.immutable_buffers.is_empty() || !self.immutable_buffers.contains(buffer))
                            {
                                if let Some(i) = array_index(k) {
                                    let view = crate::vm::typedarray::TaInterpNumericView::from_matched_non_bigint(
                                        ta_idx,
                                        *buffer,
                                        *kind,
                                        *byte_offset,
                                        *length,
                                    );
                                    // No store barrier for a number (see the
                                    // main loop's arm).
                                    if self.ta_interp_numeric_set_view(view, i, v) {
                                        ip += 1;
                                        continue;
                                    }
                                }
                            }
                        }
                    }
                    let strict = self.func(func_id as usize).is_strict;
                    self.set_index(o, k, v, strict)?;
                    ip += 1;
                }
                Instr::CallWithThis {
                    dst,
                    callee,
                    this_v,
                    arg_base,
                    argc,
                    ..
                } if FULL => {
                    // A member call in spec order whose callee is a plain user
                    // function: the main arm's frame push, lean when `this`
                    // needs no boxing. Everything else is the main arm's
                    // (natives, `f.call`, the captured intrinsic lanes).
                    let (callee_v, this_val) = (self.get(base, callee), self.get(base, this_v));
                    let Some((fid, closure)) = self.ic_plain_fn(callee_v) else {
                        hand_over!();
                    };
                    let this_is_object =
                        this_val.is_heap() && matches!(self.heap.get(this_val.heap_index()), HeapObj::Object(_));
                    self.setup_call_lean_this(
                        fid,
                        closure,
                        this_val,
                        this_is_object,
                        base,
                        arg_base,
                        argc,
                        dst,
                        ip + 1,
                        callee_v,
                    )?;
                    lite_enter!(fid);
                }
                Instr::CallMethod {
                    dst,
                    obj,
                    name,
                    arg_base,
                    argc,
                } if FULL => {
                    // An ordinary object's or class instance's method through
                    // the method cache, the main arm's first step for one; an
                    // array's `push` and a string's `charCodeAt`, its leaves.
                    // Anything else is the main arm's.
                    let recv = self.get(base, obj);
                    if !recv.is_heap() {
                        hand_over!();
                    }
                    // The key's text, for what the method cache does not
                    // answer (it reads no key on a hit).
                    macro_rules! key {
                        () => {
                            self.func(func_id as usize).string_constants[name as usize].as_str()
                        };
                    }
                    match self.heap.get(recv.heap_index()) {
                        HeapObj::Object(_) => {
                            match self.lite_call_method(&code[ip], func_id, ip, recv, name, base, arg_base, argc, dst)? {
                                LiteCallOut::Value(v) => {
                                    steps += LITE_CALL_STEPS;
                                    self.set(base, dst, v);
                                    ip += 1;
                                }
                                LiteCallOut::Pushed(fid) => lite_enter!(fid),
                                LiteCallOut::HandOver => hand_over!(),
                            }
                        }
                        HeapObj::Array(_) if argc == 1 && super::builtins::is_push_name(key!()) => {
                            let v = self.get(base, arg_base);
                            let Some(len) = self.interp_array_push_fast(recv, v) else {
                                hand_over!();
                            };
                            self.set(base, dst, len);
                            ip += 1;
                        }
                        HeapObj::Array(_) if argc == 0 && super::builtins::is_pop_name(key!()) => {
                            let Some(v) = self.interp_array_pop_fast(recv) else {
                                hand_over!();
                            };
                            self.set(base, dst, v);
                            ip += 1;
                        }
                        HeapObj::Str(_) | HeapObj::Cons { .. } if argc == 1 && key!() == "charCodeAt" => {
                            let arg = self.get(base, arg_base);
                            let Some(r) = self.interp_char_code_at_fast(recv, arg) else {
                                hand_over!();
                            };
                            self.set(base, dst, r);
                            ip += 1;
                        }
                        _ => hand_over!(),
                    }
                }
                Instr::Call {
                    dst,
                    callee,
                    arg_base,
                    argc,
                } => {
                    let callee_v = self.get(base, callee);
                    let Some((fid, closure, desc)) = self.lite_resolve(func_id, ip, callee_v) else {
                        hand_over!();
                    };
                    // The small copy's calls are a recursive function's, never
                    // a leaf's, and its registers are tight.
                    if FULL {
                        if let Some(v) = self.lite_leaf(fid, closure, Value::UNDEFINED, false, base, arg_base, argc) {
                            steps += LITE_CALL_STEPS;
                            self.set(base, dst, v);
                            ip += 1;
                            continue;
                        }
                    }
                    // Every push puts the callee's window at the file's end:
                    // its base, known before the push stores it.
                    let callee_base = self.regs.len();
                    self.setup_call_lean_desc(fid, closure, desc, base, arg_base, argc, dst, ip + 1, callee_v)?;
                    steps += LITE_CALL_STEPS;
                    // The main loop's frame-transition safe point.
                    self.maybe_gc();
                    if !self.lite_eligible(fid) {
                        // The callee runs in the main loop, and the caller
                        // after it until its next frame transition.
                        self.lite_handed_over(func_id);
                        exit!(LiteStep::Exit(LiteExit::Resume));
                    }
                    if !FULL && self.lite_is_full(fid) {
                        exit!(LiteStep::Full);
                    }
                    top += 1;
                    debug_assert_eq!(top, self.frames.len() - 1);
                    debug_assert_eq!(callee_base, self.frames[top].base);
                    func_id = fid;
                    base = callee_base;
                    ip = 0;
                    code = self.lite_fn_code(func_id);
                    #[cfg(not(feature = "safe-sandbox"))]
                    {
                        upv = (std::ptr::null(), 0);
                    }
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
                   function tiny(x) { return x + 1; } \
                   for (var t = 0, u = 0; t < 200; t++) u = tiny(u) + (typeof t === 'number' ? 0 : 1); \
                   function readP(o) { var s = 0; for (var i = 0; i < 50; i++) s = s + o.p; return s; } \
                   console.log(loopSum(10) + callsOut(200) + fib(10) + u + readP({ p: 2 }));";
        let ast = crate::front::parse_auto(src).expect("parse");
        let program = crate::compile::compile_program(&ast, src).expect("compile");
        let fid = |name: &str| program.functions.iter().position(|f| f.name == name).expect(name) as u32;
        let (lp, out, fb) = (fid("loopSum"), fid("callsOut"), fid("fib"));
        assert!(has_back_edge(&program.functions[lp as usize].code), "loopSum has a loop");
        let mut vm = Vm::new(&program);
        #[cfg(all(feature = "jit", any(target_arch = "x86_64", target_arch = "aarch64")))]
        vm.set_jit_enabled(false);
        vm.run().expect("run");
        assert_eq!(vm.output, ["600"]);
        if lite_enabled() {
            assert!(vm.lite_eligible(lp), "loopSum runs in the lite loop");
            assert!(vm.lite_eligible(fb), "fib runs in the lite loop (its entry is long)");
            assert_eq!(vm.lite.ok[fb as usize], LITE_SMALL, "fib runs in the small copy");
            assert_eq!(vm.lite.ok[fid("readP") as usize], LITE_FULL, "a property read needs the full copy");
            // Its caller's loop holds a `typeof`, so the main program runs in
            // the main loop and every call of `tiny` is a short entry.
            assert!(!vm.lite_eligible(fid("tiny")), "tiny's short entries from the main program spent its budget");
            assert!(!vm.lite_eligible(fid("other")), "typeof is not in the lite set");
            // Eligible by its instructions, so only the budget sent it back.
            let p = &program.functions[out as usize];
            assert!(p.code.iter().chain(Vm::lite_code(p)).all(lite_instr), "callsOut is in the lite set");
            assert_eq!(vm.lite.ok[out as usize], 2, "callsOut spent its hand-overs");
            assert_eq!(vm.lite.budget[out as usize], 0);
        }
    }
}