//! The loop runner: a hot loop's simple instructions through small handlers
//! that jump from one to the next.
//!
//! The dispatch loops (`dispatch_body`, the lite loop) are each one large
//! function, and an instruction there costs about fifty machine
//! instructions: the dispatch with its bounds check and table jump, the
//! fields' loads and widenings, the register file's base read again after
//! every store, and the moves a function of thousands of arms makes around
//! each of them. The processor runs those at close to its full width, so a
//! loop that only adds, compares, masks and stores takes time in proportion
//! to that count and nothing else.
//!
//! Here each instruction of a function's dispatched code (its
//! superinstruction copy, position for position) is an [`Op`]: its operands
//! decoded once, and a handler, a function of a dozen or so machine
//! instructions that does the instruction's work for the operand kinds every
//! iteration has (Int32s, doubles) and then calls the next position's
//! handler in tail position: a jump, and each handler's own, so the
//! processor predicts each on its own history. Anything a handler does not
//! do (another operand kind, an instruction with no handler) returns the
//! position itself, marked: the runner hands it back, and the dispatch loop
//! runs that instruction as it always has and comes back at the loop's next
//! back-edge. A handler changes nothing before it knows it will finish,
//! except writes the instruction repeats identically, so the dispatch loop
//! may redo it.
//!
//! A tail call is the compiler's choice, not the language's promise, and it
//! declines one in a function that has handed a callee the address of
//! anything in its frame, while a value that must outlive a call costs the
//! function a saved register on every path. So a handler keeps nothing
//! across a call. One whose common case needs none is that case alone, and
//! its other cases are a second handler it tail-calls (`slow!`). One that
//! calls into the VM is two functions (`heavy!`): a body that does the work
//! and answers the next position, and the handler proper, which calls the
//! body and then the next handler with what the runner left in the VM for
//! it. Should a call still not become a jump, the native stack is bounded
//! all the same: a back-edge counts down a budget carried in a register and
//! compares the stack pointer with the runner's own, and returns to the
//! runner's loop when either is spent. A debug build, which makes no tail
//! calls, returns after every instruction. The test below counts the
//! returns that comparison forces over loops of every handler: none, while
//! every call is a jump.
//!
//! A loop's call of a function that is not a leaf is the runner's as well.
//! It pushes the callee's frame as the lite loop does (the lean push only:
//! any other call is the dispatch loop's), and the handlers go on at the
//! callee's first position, in its window, each function's positions built
//! at its first use; a `Return` there pops the frame and goes on after the
//! call. The frame the runner was entered in returns in the dispatch loop.
//! Whatever leaves the runner in a frame it pushed stores its position in
//! that frame, and the dispatch loop goes on from the top frame, as after
//! any call (`LOOP_FRAMES`).
//!
//! Only the interpreter-only dispatch loop enters it (`LITE`), at a
//! back-edge of a loop that has run a while, and only on x86-64, where
//! every hand-off is checked to be a jump (`Vm::loop_warm`).

use super::dispatch::number_int32;
use super::*;
use crate::bytecode::{BitwiseOp, FuncProto, Instr};
use core::ffi::c_void;

/// What a handler returns: where to go on (bit 0 set: leave at this
/// position, for the dispatch loop), and the back-edges left in the budget.
#[repr(C)]
#[derive(Clone, Copy)]
struct Step {
    next: usize,
    fuel: u64,
}

/// What a `heavy!` handler's body returns: where to go on, as [`Step`]'s,
/// and the VM it was given (all its caller needs back).
#[repr(C)]
#[derive(Clone, Copy)]
struct Done {
    next: usize,
    vm: *mut c_void,
}

/// The Int32 tag: an Int is these bits over its 32-bit payload.
const INT: u64 = 0x7ff9_0000_0000_0000;
const BOOL: u64 = 0x7ffa_0000_0000_0000;
/// A global slot never initialized: the dispatch arms' slow paths.
const UNINIT: u64 = Value::UNINITIALIZED.bits();

/// Back-edges a handler chain takes before it returns to the runner's loop.
const FUEL: u64 = 4096;

/// A handler: the VM, the frame's first register, the position and the
/// budget. The System V convention on x86-64 Windows too: its arguments in
/// registers that leave five more free, and the two words of a [`Step`]
/// back in registers.
#[cfg(target_arch = "x86_64")]
type Handler = unsafe extern "sysv64" fn(vm: *mut c_void, regs: *mut u64, op: *const Op, fuel: u64) -> Step;
#[cfg(not(target_arch = "x86_64"))]
type Handler = unsafe extern "C" fn(vm: *mut c_void, regs: *mut u64, op: *const Op, fuel: u64) -> Step;

/// One position: its handler and its operands (`a` and `b` source
/// registers, `c` the destination, `d` a jump target as a byte offset from
/// this position or a second destination, `k` a literal's bits or an
/// immediate). What each means is its handler's.
#[repr(C)]
pub(crate) struct Op {
    h: Handler,
    a: u32,
    b: u32,
    c: u32,
    d: u32,
    k: u64,
}

/// A handler's arguments, as one value.
#[derive(Clone, Copy)]
struct Cx {
    vm: *mut c_void,
    regs: *mut u64,
    op: *const Op,
    fuel: u64,
}

impl Cx {
    #[inline(always)]
    unsafe fn a(self) -> u32 {
        (*self.op).a
    }
    #[inline(always)]
    unsafe fn b(self) -> u32 {
        (*self.op).b
    }
    #[inline(always)]
    unsafe fn c(self) -> u32 {
        (*self.op).c
    }
    #[inline(always)]
    unsafe fn d(self) -> u32 {
        (*self.op).d
    }
    #[inline(always)]
    unsafe fn k(self) -> u64 {
        (*self.op).k
    }
    /// Register `r`'s bits.
    #[inline(always)]
    unsafe fn get(self, r: u32) -> u64 {
        *self.regs.add(r as usize)
    }
    #[inline(always)]
    unsafe fn put(self, r: u32, bits: u64) {
        *self.regs.add(r as usize) = bits;
    }
    /// The position `n` instructions on.
    #[inline(always)]
    unsafe fn after(self, n: usize) -> *const Op {
        self.op.add(n)
    }
    /// The position this one's `d` names.
    #[inline(always)]
    unsafe fn target(self) -> *const Op {
        (self.op as *const u8).offset((*self.op).d as i32 as isize) as *const Op
    }
    /// The VM. A handler that takes it calls only what runs no program
    /// code, raises nothing and moves no register file: the leaves the
    /// dispatch arms take first.
    #[inline(always)]
    #[allow(clippy::mut_from_ref)]
    unsafe fn vm<'a, 'p>(self) -> &'a mut Vm<'p> {
        &mut *(self.vm as *mut Vm<'p>)
    }
    /// Global slot `idx` (in range: `decode`).
    #[inline(always)]
    unsafe fn global(self, idx: u32) -> *mut u64 {
        (self.vm().globals.as_mut_ptr() as *mut u64).add(idx as usize)
    }
    /// This position's index: the instruction's `ip`.
    #[inline(always)]
    unsafe fn ip(self) -> usize {
        (self.op as usize - self.vm().lite.loops.ops) / std::mem::size_of::<Op>()
    }
    /// The index in the register file of the frame's first register.
    #[inline(always)]
    unsafe fn frame_base(self) -> usize {
        (self.regs as usize - self.vm().regs.as_ptr() as usize) / std::mem::size_of::<Value>()
    }
}

/// Whether `bits` are an Int32's.
#[inline(always)]
fn is_int(bits: u64) -> bool {
    (bits ^ INT) >> 32 == 0
}

/// Whether both are Int32s.
#[inline(always)]
fn ints(x: u64, y: u64) -> bool {
    ((x ^ INT) | (y ^ INT)) >> 32 == 0
}

#[inline(always)]
fn int(v: i32) -> u64 {
    INT | v as u32 as u64
}

/// Whether both are doubles (neither top sixteen bits a tag's).
#[inline(always)]
fn doubles(x: u64, y: u64) -> bool {
    (x >> 48).wrapping_sub(0x7ff9) > 4 && (y >> 48).wrapping_sub(0x7ff9) > 4
}

/// Go on at `n`: its handler, in tail position.
#[cfg(not(debug_assertions))]
#[inline(always)]
unsafe fn go(cx: Cx, n: *const Op) -> Step {
    ((*n).h)(cx.vm, cx.regs, n, cx.fuel)
}

/// Go on at `n`: back to the runner's loop, which calls it (an unoptimized
/// build makes no tail calls).
#[cfg(debug_assertions)]
#[inline(always)]
unsafe fn go(cx: Cx, n: *const Op) -> Step {
    Step {
        next: n as usize,
        fuel: cx.fuel,
    }
}

/// The stack pointer, where reading it costs nothing (elsewhere the
/// budget alone bounds the stack).
#[inline(always)]
fn stack_ptr() -> usize {
    #[cfg(target_arch = "x86_64")]
    {
        let sp: usize;
        // SAFETY: reads a register.
        unsafe { core::arch::asm!("mov {}, rsp", out(reg) sp, options(nomem, nostack, preserves_flags)) };
        sp
    }
    #[cfg(target_arch = "aarch64")]
    {
        let sp: usize;
        // SAFETY: reads a register.
        unsafe { core::arch::asm!("mov {}, sp", out(reg) sp, options(nomem, nostack, preserves_flags)) };
        sp
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        usize::MAX
    }
}

/// Native stack a handler chain may take below the runner's loop before a
/// back-edge returns to it: none, when its calls are jumps.
const STACK_SLACK: usize = 32 << 10;

/// Take a back-edge to `n`: one off the budget, and back to the runner's
/// loop when it is spent or the chain has grown the native stack.
#[inline(always)]
unsafe fn back(mut cx: Cx, n: *const Op) -> Step {
    cx.fuel -= 1;
    if cx.fuel == 0 {
        return Step { next: n as usize, fuel: 0 };
    }
    if stack_ptr() < cx.vm().lite.loops.stack_floor {
        cx.vm().lite.loops.stack_trips += 1;
        return Step {
            next: n as usize,
            fuel: cx.fuel,
        };
    }
    go(cx, n)
}

/// Leave at this position.
#[inline(always)]
fn out(cx: Cx) -> Step {
    Step {
        next: cx.op as usize | LEAVE,
        fuel: cx.fuel,
    }
}

/// Bits of the position a handler answers (positions are eight-aligned):
/// leave at it, for the dispatch loop; and with that, the handler's work
/// raised an error (`LoopTier::thrown`), which the dispatch loop returns.
const LEAVE: usize = 1;
const THREW: usize = 2;
/// With `LEAVE`: the handlers stopped for the dispatch loop's sake (a
/// collection is due at its next safe point, or program code changed what
/// the entry was told), not at an instruction they do not run: no strike.
const PAUSE: usize = 4;

/// A `heavy!` body's answer when its work raised `e`.
#[cold]
#[inline(never)]
unsafe fn raise(cx: Cx, e: Thrown) -> Done {
    cx.vm().lite.loops.thrown = Some(e);
    Done {
        next: cx.op as usize | LEAVE | THREW,
        vm: cx.vm,
    }
}

/// After a `heavy!` body's call of an operation that may run program code
/// (a native with a callback, a getter, a conversion): the frame's
/// registers where they are now (a nested run may have moved the register
/// file; the runner's other state a nested entry puts back itself), and
/// whether the handlers may go on, which they may not once that code has
/// changed how a global is reached (`Vm::loop_globals_plain`, asked at the
/// entry).
#[inline(always)]
unsafe fn settle(cx: Cx) -> (Cx, bool) {
    let vm = cx.vm();
    let base = vm.frames[vm.frames.len() - 1].base;
    let regs = vm.regs.as_mut_ptr().add(base) as *mut u64;
    vm.lite.loops.regs = regs as usize;
    (Cx { regs, ..cx }, vm.loop_globals_plain() && !vm.heap.gc_requested())
}

/// The answer of a `heavy!` body whose work is done: on at `n`, or (`on`
/// false) leave there with no strike.
#[inline(always)]
fn then(cx: Cx, n: *const Op, on: bool) -> Done {
    Done {
        next: n as usize | if on { 0 } else { LEAVE | PAUSE },
        vm: cx.vm,
    }
}

/// A `heavy!` body's answer after work that allocated and ran no code: on
/// at `n`, or leave there for the collection that is now due, which the
/// dispatch loop runs at its next safe point (the handlers' own is every
/// few thousand back-edges, `FUEL`).
#[inline(always)]
unsafe fn after_alloc(cx: Cx, n: *const Op) -> Done {
    then(cx, n, !cx.vm().heap.gc_requested())
}

/// A `heavy!` body's answer after such an operation: on at the next
/// position, or leave there (`settle`); its error, when it raised one.
#[inline(always)]
unsafe fn finish_unit(cx: Cx, r: Result<(), Thrown>) -> Done {
    let (cx, on) = settle(cx);
    match r {
        Ok(()) => then(cx, cx.after(1), on),
        Err(e) => raise(cx, e),
    }
}

/// [`finish_unit`] for an operation with a result, which goes to `c`.
#[inline(always)]
unsafe fn finish(cx: Cx, r: Result<Value, Thrown>) -> Done {
    let (cx, on) = settle(cx);
    match r {
        Ok(v) => {
            cx.put(cx.c(), v.bits());
            then(cx, cx.after(1), on)
        }
        Err(e) => raise(cx, e),
    }
}

/// Declare a handler: `h!(name, |cx| { … })`, the body answering a [`Step`].
macro_rules! h {
    ($(#[$m:meta])* $name:ident, |$cx:ident| $body:block) => {
        $(#[$m])*
        #[cfg(target_arch = "x86_64")]
        unsafe extern "sysv64" fn $name(vm: *mut c_void, regs: *mut u64, op: *const Op, fuel: u64) -> Step {
            let $cx = Cx { vm, regs, op, fuel };
            $body
        }
        $(#[$m])*
        #[cfg(not(target_arch = "x86_64"))]
        unsafe extern "C" fn $name(vm: *mut c_void, regs: *mut u64, op: *const Op, fuel: u64) -> Step {
            let $cx = Cx { vm, regs, op, fuel };
            $body
        }
    };
}

/// Declare a handler's other cases, which it tail-calls
/// (`again!(cx, name)`): a handler in its own right, out of line.
macro_rules! slow {
    ($name:ident, |$cx:ident| $body:block) => {
        h!(
            #[cold]
            #[inline(never)]
            $name,
            |$cx| $body
        );
    };
}

/// Hand this position to another of its handlers, in tail position.
macro_rules! again {
    ($cx:ident, $name:ident) => {
        $name($cx.vm, $cx.regs, $cx.op, $cx.fuel)
    };
}

/// Declare a handler whose work calls into the VM, as two functions (see
/// the module doc): `heavy!(name, body_name, |cx, go, out| { … })`. In the
/// block `go(cx, n)` and `out(cx)` answer the body's result (the next
/// position, or this one to leave at) rather than taking the step. The
/// handler proper keeps nothing across the body's call: the budget goes to
/// the VM before it, and after it the frame's registers and the budget come
/// back from the VM the body returns.
macro_rules! heavy {
    ($(#[$m:meta])* $name:ident, $body:ident, |$cx:ident, $go:ident, $out:ident| $code:block) => {
        #[cfg(target_arch = "x86_64")]
        #[inline(never)]
        unsafe extern "sysv64" fn $body(vm: *mut c_void, regs: *mut u64, op: *const Op) -> Done {
            let $cx = Cx { vm, regs, op, fuel: 0 };
            #[allow(unused_variables)]
            let $go = |c: Cx, n: *const Op| Done { next: n as usize, vm: c.vm };
            #[allow(unused_variables)]
            let $out = |c: Cx| Done { next: c.op as usize | LEAVE, vm: c.vm };
            $code
        }
        #[cfg(not(target_arch = "x86_64"))]
        #[inline(never)]
        unsafe extern "C" fn $body(vm: *mut c_void, regs: *mut u64, op: *const Op) -> Done {
            let $cx = Cx { vm, regs, op, fuel: 0 };
            #[allow(unused_variables)]
            let $go = |c: Cx, n: *const Op| Done { next: n as usize, vm: c.vm };
            #[allow(unused_variables)]
            let $out = |c: Cx| Done { next: c.op as usize | LEAVE, vm: c.vm };
            $code
        }
        h!(
            $(#[$m])*
            $name,
            |cx| {
                (*(cx.vm as *mut Vm)).lite.loops.fuel = cx.fuel;
                let done = $body(cx.vm, cx.regs, cx.op);
                let loops = &(*(done.vm as *mut Vm)).lite.loops;
                let fuel = loops.fuel;
                if done.next & LEAVE != 0 {
                    return Step { next: done.next, fuel };
                }
                let cx = Cx { vm: done.vm, regs: loops.regs as *mut u64, op: done.next as *const Op, fuel };
                go(cx, done.next as *const Op)
            }
        );
    };
}

h!(
    /// No handler: the dispatch loop's instruction.
    h_out,
    |cx| { out(cx) }
);

h!(
    /// `c = k`: `LoadInt`, a numeric `LoadConst`, `LoadBool`, `LoadNull`,
    /// `LoadUndefined`.
    h_load,
    |cx| {
        cx.put(cx.c(), cx.k());
        go(cx, cx.after(1))
    }
);

h!(h_move, |cx| {
    cx.put(cx.c(), cx.get(cx.a()));
    go(cx, cx.after(1))
});

h!(
    /// `FuseMove2`: `c = a; d = b`.
    h_move2,
    |cx| {
        cx.put(cx.c(), cx.get(cx.a()));
        cx.put(cx.d(), cx.get(cx.b()));
        go(cx, cx.after(2))
    }
);

/// `c = a + b`, `a - b`, `a * b` on two Numbers, as the dispatch arms make
/// them: an Int32 result when two Int32s give one, else the double. Two
/// Int32s whose result fits and two doubles are the handler's; a result
/// that does not fit and a mixed pair are `$slow`'s.
macro_rules! arith {
    ($name:ident, $slow:ident, $other:ident, $checked:ident, $fop:tt, $int_ok:expr) => {
        slow!($slow, |cx| {
            let (vx, vy) = (Value::from_bits(cx.get(cx.a())), Value::from_bits(cx.get(cx.b())));
            if !(vx.is_number() && vy.is_number()) {
                return again!(cx, $other);
            }
            cx.put(cx.c(), Value::num(vx.as_f64() $fop vy.as_f64()).bits());
            go(cx, cx.after(1))
        });
        h!($name, |cx| {
            let (x, y) = (cx.get(cx.a()), cx.get(cx.b()));
            if ints(x, y) {
                let (ix, iy) = (x as i32, y as i32);
                let ok: fn(i32, i32, i32) -> bool = $int_ok;
                if let Some(v) = ix.$checked(iy) {
                    if ok(ix, iy, v) {
                        cx.put(cx.c(), int(v));
                        return go(cx, cx.after(1));
                    }
                }
            } else if doubles(x, y) {
                cx.put(cx.c(), Value::num(f64::from_bits(x) $fop f64::from_bits(y)).bits());
                return go(cx, cx.after(1));
            }
            again!(cx, $slow)
        });
    };
}
arith!(h_add, s_add, h_add_any, checked_add, +, |_, _, _| true);
arith!(h_sub, s_sub, h_out, checked_sub, -, |_, _, _| true);
// A zero product with a negative operand is -0, a double: the arm's.
arith!(h_mul, s_mul, h_out, checked_mul, *, |x, y, v| v != 0 || (x | y) >= 0);

/// Whether `+` on `x` and `y` runs no program code: neither is an object,
/// whose conversion calls its `valueOf` or `toString`, nor a Symbol (a
/// TypeError, raised before anything runs, but kept with the general case).
/// Then nothing can move the register file or change how a global is
/// reached, and the handler goes on by `after_alloc`, not `finish`.
#[inline(always)]
fn add_runs_no_code(vm: &Vm, x: Value, y: Value) -> bool {
    (!x.is_heap() || vm.heap.is_str_like(x.heap_index())) && (!y.is_heap() || vm.heap.is_str_like(y.heap_index()))
}

/// A `heavy!` body's answer after `+` that ran no program code
/// (`add_runs_no_code`): the result to `c`, on by `after_alloc`.
#[inline(always)]
unsafe fn added(cx: Cx, r: Result<Value, Thrown>) -> Done {
    match r {
        Ok(v) => {
            cx.put(cx.c(), v.bits());
            after_alloc(cx, cx.after(1))
        }
        Err(e) => raise(cx, e),
    }
}

heavy!(
    /// `Add` on operands that are not two Numbers: the general `+` (a
    /// string's concatenation, an object's conversion, BigInts).
    h_add_any,
    b_add_any,
    |cx, go, out| {
        let vm = cx.vm();
        let (x, y) = (Value::from_bits(cx.get(cx.a())), Value::from_bits(cx.get(cx.b())));
        if add_runs_no_code(vm, x, y) {
            return added(cx, vm.add_values(x, y));
        }
        finish(cx, vm.add_values(x, y))
    }
);

heavy!(
    /// `StrConcatChain`: `c = a + b`, a link of a concatenation chain whose
    /// accumulator may grow in place, as the dispatch arm makes it.
    h_str_chain,
    b_str_chain,
    |cx, go, out| {
        let vm = cx.vm();
        let (x, y) = (Value::from_bits(cx.get(cx.a())), Value::from_bits(cx.get(cx.b())));
        if add_runs_no_code(vm, x, y) {
            return added(cx, vm.add_values_chain(x, y));
        }
        finish(cx, vm.add_values_chain(x, y))
    }
);

heavy!(
    /// `StrAppendInPlace`: `c = a + b` where `a` is the statement's own
    /// accumulator (`s += x`), as the dispatch arm makes it: the append in
    /// place, or the general `+` when `b` needs a conversion.
    h_str_append,
    b_str_append,
    |cx, go, out| {
        let vm = cx.vm();
        let (x, y) = (Value::from_bits(cx.get(cx.a())), Value::from_bits(cx.get(cx.b())));
        // `str_append_inplace` answers only when `y` needs no conversion.
        if let Some(r) = vm.str_append_inplace(x, y) {
            return added(cx, Ok(r));
        }
        if add_runs_no_code(vm, x, y) {
            return added(cx, vm.add_values(x, y));
        }
        finish(cx, vm.add_values(x, y))
    }
);

heavy!(
    /// `ToStr`: `c = ToString(a)`, a template literal's substitution.
    h_to_str,
    b_to_str,
    |cx, go, out| {
        let vm = cx.vm();
        let x = Value::from_bits(cx.get(cx.a()));
        // A primitive's or a string's ToString runs no program code.
        if add_runs_no_code(vm, x, x) {
            return added(cx, vm.to_str_value(x));
        }
        finish(cx, vm.to_str_value(x))
    }
);

slow!(s_div, |cx| {
    let (vx, vy) = (Value::from_bits(cx.get(cx.a())), Value::from_bits(cx.get(cx.b())));
    if !(vx.is_number() && vy.is_number()) {
        return out(cx);
    }
    cx.put(cx.c(), Value::num(vx.as_f64() / vy.as_f64()).bits());
    go(cx, cx.after(1))
});

h!(
    /// `Div`: `c = a / b` on two doubles; two Numbers otherwise are
    /// `s_div`'s.
    h_div,
    |cx| {
        let (x, y) = (cx.get(cx.a()), cx.get(cx.b()));
        if doubles(x, y) {
            cx.put(cx.c(), Value::num(f64::from_bits(x) / f64::from_bits(y)).bits());
            return go(cx, cx.after(1));
        }
        again!(cx, s_div)
    }
);

heavy!(
    /// `Mod`: `c = a % b` on two Numbers.
    h_mod,
    b_mod,
    |cx, go, out| {
        let (x, y) = (cx.get(cx.a()), cx.get(cx.b()));
        let (vx, vy) = (Value::from_bits(x), Value::from_bits(y));
        let r = if ints(x, y) {
            super::helpers_num2::js_rem_int(x as i32, y as i32)
        } else if vx.is_number() && vy.is_number() {
            Value::num(super::helpers_num2::js_rem(vx.as_f64(), vy.as_f64()))
        } else {
            return out(cx);
        };
        cx.put(cx.c(), r.bits());
        go(cx, cx.after(1))
    }
);

/// `x + k` for a Number `x` and an Int32 literal's bits `k`, as `AddInt`
/// and its superinstructions make it (`Vm::add_int_value`): `None` for
/// anything but a Number.
#[inline(always)]
fn add_lit(x: u64, k: u64) -> Option<u64> {
    if is_int(x) {
        return Some(match (x as i32).checked_add(k as i32) {
            Some(v) => int(v),
            None => Value::num(x as i32 as f64 + k as i32 as f64).bits(),
        });
    }
    let vx = Value::from_bits(x);
    vx.is_double().then(|| Value::num(vx.as_f64() + k as i32 as f64).bits())
}

/// `x + k` on an Int32 whose sum is one: the handlers' own case of
/// [`add_lit`].
#[inline(always)]
fn add_lit_int(x: u64, k: u64) -> Option<u64> {
    if !is_int(x) {
        return None;
    }
    (x as i32).checked_add(k as i32).map(int)
}

slow!(s_add_int, |cx| {
    let Some(r) = add_lit(cx.get(cx.a()), cx.k()) else { return out(cx) };
    cx.put(cx.c(), r);
    go(cx, cx.after(1))
});

h!(
    /// `AddInt`: `c = a + k` on a Number.
    h_add_int,
    |cx| {
        let Some(r) = add_lit_int(cx.get(cx.a()), cx.k()) else { return again!(cx, s_add_int) };
        cx.put(cx.c(), r);
        go(cx, cx.after(1))
    }
);

slow!(s_add_int_move, |cx| {
    let Some(r) = add_lit(cx.get(cx.a()), cx.k()) else { return out(cx) };
    cx.put(cx.c(), r);
    cx.put(cx.d(), cx.get(cx.b()));
    go(cx, cx.after(2))
});

h!(
    /// `FuseAddIntMove`: `c = a + k` as `h_add_int`, then `d = b`.
    h_add_int_move,
    |cx| {
        let Some(r) = add_lit_int(cx.get(cx.a()), cx.k()) else { return again!(cx, s_add_int_move) };
        cx.put(cx.c(), r);
        cx.put(cx.d(), cx.get(cx.b()));
        go(cx, cx.after(2))
    }
);

slow!(s_add_load_int, |cx| {
    let (x, k) = (cx.get(cx.a()), cx.k());
    cx.put(cx.b(), k);
    let Some(r) = add_lit(x, k) else { return out(cx) };
    cx.put(cx.c(), r);
    go(cx, cx.after(2))
});

h!(
    /// `FuseAddLoadInt`: `b = k; c = a + k` (`a` is not `b`: `decode`).
    h_add_load_int,
    |cx| {
        let (x, k) = (cx.get(cx.a()), cx.k());
        let Some(r) = add_lit_int(x, k) else { return again!(cx, s_add_load_int) };
        cx.put(cx.b(), k);
        cx.put(cx.c(), r);
        go(cx, cx.after(2))
    }
);

/// Two Numbers as the Int32s the bitwise operators take (`number_int32`:
/// a double below 2^63 truncates and wraps): `None` for NaN, an infinity,
/// a larger magnitude and anything that is not a Number.
#[inline(always)]
fn int32s(x: u64, y: u64) -> Option<(i32, i32)> {
    number_int32(Value::from_bits(x)).zip(number_int32(Value::from_bits(y)))
}

/// One bitwise operator's four handlers and their other cases. `$reg`:
/// `Bitwise` on two registers. `$lit`: `FuseLoadIntBitwise` whose literal
/// is the right operand (`b = k; c = a OP k`, `a` not `b`). `$glob`:
/// `FuseGlobalIntBitwise` (`a = globals[d]; b = k; c = a OP k`). `$store`:
/// `FuseIntBitwiseStoreGlobal` (`b = k; c = a OP k; globals[d] = c`, `a`
/// not `b`, into a slot a plain store writes). Two Int32s whose result is
/// one are each handler's own case; Numbers ToInt32 takes without the
/// general conversion (`Vm::fused_bitwise`'s inline cases), and a result
/// that is a double, are its `s_` twin's.
macro_rules! bitwise_ops {
    ($($reg:ident, $sreg:ident, $lit:ident, $slit:ident, $glob:ident, $sglob:ident, $store:ident, $sstore:ident,
       $fast:expr, $full:expr;)*) => {
        $(
            slow!($sreg, |cx| {
                let Some((ix, iy)) = int32s(cx.get(cx.a()), cx.get(cx.b())) else { return out(cx) };
                let f: fn(i32, i32) -> Value = $full;
                cx.put(cx.c(), f(ix, iy).bits());
                go(cx, cx.after(1))
            });
            h!($reg, |cx| {
                let (x, y) = (cx.get(cx.a()), cx.get(cx.b()));
                let f: fn(i32, i32) -> Option<i32> = $fast;
                if ints(x, y) {
                    if let Some(v) = f(x as i32, y as i32) {
                        cx.put(cx.c(), int(v));
                        return go(cx, cx.after(1));
                    }
                }
                again!(cx, $sreg)
            });
            slow!($slit, |cx| {
                let (x, k) = (cx.get(cx.a()), cx.k());
                cx.put(cx.b(), k);
                let Some((ix, ik)) = int32s(x, k) else { return out(cx) };
                let f: fn(i32, i32) -> Value = $full;
                cx.put(cx.c(), f(ix, ik).bits());
                go(cx, cx.after(2))
            });
            h!($lit, |cx| {
                let (x, k) = (cx.get(cx.a()), cx.k());
                let f: fn(i32, i32) -> Option<i32> = $fast;
                if is_int(x) {
                    if let Some(v) = f(x as i32, k as i32) {
                        cx.put(cx.b(), k);
                        cx.put(cx.c(), int(v));
                        return go(cx, cx.after(2));
                    }
                }
                again!(cx, $slit)
            });
            slow!($sglob, |cx| {
                let (g, k) = (*cx.global(cx.d()), cx.k());
                let Some((ig, ik)) = int32s(g, k) else { return out(cx) };
                let f: fn(i32, i32) -> Value = $full;
                cx.put(cx.a(), g);
                cx.put(cx.b(), k);
                cx.put(cx.c(), f(ig, ik).bits());
                go(cx, cx.after(3))
            });
            h!($glob, |cx| {
                let (g, k) = (*cx.global(cx.d()), cx.k());
                let f: fn(i32, i32) -> Option<i32> = $fast;
                if is_int(g) {
                    if let Some(v) = f(g as i32, k as i32) {
                        cx.put(cx.a(), g);
                        cx.put(cx.b(), k);
                        cx.put(cx.c(), int(v));
                        return go(cx, cx.after(3));
                    }
                }
                again!(cx, $sglob)
            });
            slow!($sstore, |cx| {
                let (x, k) = (cx.get(cx.a()), cx.k());
                let slot = cx.global(cx.d());
                if *slot == UNINIT {
                    return out(cx);
                }
                let Some((ix, ik)) = int32s(x, k) else { return out(cx) };
                let f: fn(i32, i32) -> Value = $full;
                let r = f(ix, ik).bits();
                cx.put(cx.b(), k);
                cx.put(cx.c(), r);
                *slot = r;
                go(cx, cx.after(3))
            });
            h!($store, |cx| {
                let (x, k) = (cx.get(cx.a()), cx.k());
                let slot = cx.global(cx.d());
                let f: fn(i32, i32) -> Option<i32> = $fast;
                if is_int(x) && *slot != UNINIT {
                    if let Some(v) = f(x as i32, k as i32) {
                        let r = int(v);
                        cx.put(cx.b(), k);
                        cx.put(cx.c(), r);
                        *slot = r;
                        return go(cx, cx.after(3));
                    }
                }
                again!(cx, $sstore)
            });
        )*
    };
}
bitwise_ops! {
    h_and, s_and, h_li_and, s_li_and, h_gl_and, s_gl_and, h_st_and, s_st_and,
        |x, y| Some(x & y), |x, y| Value::int(x & y);
    h_or, s_or, h_li_or, s_li_or, h_gl_or, s_gl_or, h_st_or, s_st_or,
        |x, y| Some(x | y), |x, y| Value::int(x | y);
    h_xor, s_xor, h_li_xor, s_li_xor, h_gl_xor, s_gl_xor, h_st_xor, s_st_xor,
        |x, y| Some(x ^ y), |x, y| Value::int(x ^ y);
    h_shl, s_shl, h_li_shl, s_li_shl, h_gl_shl, s_gl_shl, h_st_shl, s_st_shl,
        |x, y| Some(x.wrapping_shl(y as u32 & 31)), |x, y| Value::int(x.wrapping_shl(y as u32 & 31));
    h_shr, s_shr, h_li_shr, s_li_shr, h_gl_shr, s_gl_shr, h_st_shr, s_st_shr,
        |x, y| Some(x >> (y as u32 & 31)), |x, y| Value::int(x >> (y as u32 & 31));
    // `>>>` answers a uint32: an Int32 below 2^31, a double above.
    h_ushr, s_ushr, h_li_ushr, s_li_ushr, h_gl_ushr, s_gl_ushr, h_st_ushr, s_st_ushr,
        |x, y| {
            let u = (x as u32) >> (y as u32 & 31);
            (u <= i32::MAX as u32).then_some(u as i32)
        },
        |x, y| {
            let u = (x as u32) >> (y as u32 & 31);
            if u <= i32::MAX as u32 {
                Value::int(u as i32)
            } else {
                Value::num(u as f64)
            }
        };
}

/// The handler of `op` in a family of six (`[and, or, xor, shl, shr, ushr]`).
fn bitwise_handler(op: BitwiseOp, family: [Handler; 6]) -> Handler {
    family[match op {
        BitwiseOp::And => 0,
        BitwiseOp::Or => 1,
        BitwiseOp::Xor => 2,
        BitwiseOp::Shl => 3,
        BitwiseOp::Shr => 4,
        BitwiseOp::Ushr => 5,
    }]
}

/// Whether two values are strictly equal, as the dispatch arms decide it
/// (`Vm::strict_eq_fast`: two strings by their text).
#[inline(always)]
unsafe fn strict_eq(cx: Cx, x: u64, y: u64) -> bool {
    cx.vm().strict_eq_fast(Value::from_bits(x), Value::from_bits(y))
}

slow!(s_eq, |cx| {
    let eq = strict_eq(cx, cx.get(cx.a()), cx.get(cx.b()));
    cx.put(cx.c(), BOOL | (eq as u64 ^ cx.k()));
    go(cx, cx.after(1))
});

h!(
    /// `Eq` / `Ne`: `c = (a === b)`, negated when `k` is 1. Two Int32s are
    /// its own case.
    h_eq,
    |cx| {
        let (x, y) = (cx.get(cx.a()), cx.get(cx.b()));
        if !ints(x, y) {
            return again!(cx, s_eq);
        }
        cx.put(cx.c(), BOOL | ((x == y) as u64 ^ cx.k()));
        go(cx, cx.after(1))
    }
);

/// A relational operator on two Numbers that are not both Int32s: `None`
/// for anything else.
macro_rules! number_cmp {
    ($x:expr, $y:expr, $cmp:tt) => {{
        let (vx, vy) = (Value::from_bits($x), Value::from_bits($y));
        if vx.is_number() && vy.is_number() {
            Some(vx.as_f64() $cmp vy.as_f64())
        } else {
            None
        }
    }};
}

/// `Lt`, `Le`, `Gt`, `Ge` on two Numbers: `c = a OP b`.
macro_rules! relational {
    ($name:ident, $slow:ident, $cmp:tt) => {
        slow!($slow, |cx| {
            let Some(r) = number_cmp!(cx.get(cx.a()), cx.get(cx.b()), $cmp) else { return out(cx) };
            cx.put(cx.c(), BOOL | r as u64);
            go(cx, cx.after(1))
        });
        h!($name, |cx| {
            let (x, y) = (cx.get(cx.a()), cx.get(cx.b()));
            if !ints(x, y) {
                return again!(cx, $slow);
            }
            cx.put(cx.c(), BOOL | ((x as i32) $cmp (y as i32)) as u64);
            go(cx, cx.after(1))
        });
    };
}
relational!(h_lt, s_lt, <);
relational!(h_le, s_le, <=);
relational!(h_gt, s_gt, >);
relational!(h_ge, s_ge, >=);

/// `JumpIfNotLt` / `JumpIfNotLe` on two Numbers: on to the next position
/// when `a OP b`, else to the target.
macro_rules! cmp_jump {
    ($name:ident, $slow:ident, $cmp:tt) => {
        slow!($slow, |cx| {
            let Some(r) = number_cmp!(cx.get(cx.a()), cx.get(cx.b()), $cmp) else { return out(cx) };
            go(cx, if r { cx.after(1) } else { cx.target() })
        });
        h!($name, |cx| {
            let (x, y) = (cx.get(cx.a()), cx.get(cx.b()));
            if !ints(x, y) {
                return again!(cx, $slow);
            }
            go(cx, if (x as i32) $cmp (y as i32) { cx.after(1) } else { cx.target() })
        });
    };
}
cmp_jump!(h_jump_not_lt, s_jump_not_lt, <);
cmp_jump!(h_jump_not_le, s_jump_not_le, <=);

/// `FuseIntCmpJump`: `b = k`, then on two positions when `a OP k`, else to
/// the target (`a` is not `b`).
macro_rules! int_cmp_jump {
    ($name:ident, $slow:ident, $cmp:tt) => {
        slow!($slow, |cx| {
            let (x, k) = (cx.get(cx.a()), cx.k());
            cx.put(cx.b(), k);
            let vx = Value::from_bits(x);
            if !vx.is_double() {
                return out(cx);
            }
            go(cx, if vx.as_f64() $cmp (k as i32 as f64) { cx.after(2) } else { cx.target() })
        });
        h!($name, |cx| {
            let (x, k) = (cx.get(cx.a()), cx.k());
            if !is_int(x) {
                return again!(cx, $slow);
            }
            cx.put(cx.b(), k);
            go(cx, if (x as i32) $cmp (k as i32) { cx.after(2) } else { cx.target() })
        });
    };
}
int_cmp_jump!(h_int_lt_jump, s_int_lt_jump, <);
int_cmp_jump!(h_int_le_jump, s_int_le_jump, <=);

/// `a === k` for an Int32 literal's bits `k`: an Int of that value or a
/// double equal to it.
#[inline(always)]
fn eq_lit(x: u64, k: u64) -> bool {
    let vx = Value::from_bits(x);
    if is_int(x) {
        x == k
    } else {
        vx.is_double() && vx.as_f64() == k as i32 as f64
    }
}

h!(
    /// `FuseIntEqJump`: `b = k; c = (a === k)`; to the target when equal,
    /// else on three positions.
    h_int_eq_jump,
    |cx| {
        let (x, k) = (cx.get(cx.a()), cx.k());
        cx.put(cx.b(), k);
        let r = eq_lit(x, k);
        cx.put(cx.c(), BOOL | r as u64);
        go(cx, if r { cx.target() } else { cx.after(3) })
    }
);

h!(
    /// `FuseIntNeJump`: as `h_int_eq_jump`, to the target when not equal.
    h_int_ne_jump,
    |cx| {
        let (x, k) = (cx.get(cx.a()), cx.k());
        cx.put(cx.b(), k);
        let r = eq_lit(x, k);
        cx.put(cx.c(), BOOL | r as u64);
        go(cx, if r { cx.after(3) } else { cx.target() })
    }
);

slow!(s_eq_jump, |cx| {
    let eq = strict_eq(cx, cx.get(cx.a()), cx.get(cx.b()));
    let k = cx.k();
    let r = eq as u64 ^ (k & 1);
    cx.put(cx.c(), BOOL | r);
    go(cx, if r == k >> 1 { cx.target() } else { cx.after(2) })
});

h!(
    /// `FuseEqJump`: `c = (a === b)` negated when bit 0 of `k`; to the
    /// target (forward) when that equals bit 1 of `k`, else on two. Two
    /// Int32s are its own case.
    h_eq_jump,
    |cx| {
        let (x, y) = (cx.get(cx.a()), cx.get(cx.b()));
        if !ints(x, y) {
            return again!(cx, s_eq_jump);
        }
        let k = cx.k();
        let r = (x == y) as u64 ^ (k & 1);
        cx.put(cx.c(), BOOL | r);
        go(cx, if r == k >> 1 { cx.target() } else { cx.after(2) })
    }
);

/// What a branch on `bits` takes (`Vm::jump_condition`).
#[inline(always)]
unsafe fn truth(cx: Cx, bits: u64) -> bool {
    cx.vm().jump_condition(Value::from_bits(bits))
}

/// Whether `bits` are a boolean's.
#[inline(always)]
fn is_bool(bits: u64) -> bool {
    bits >> 1 == BOOL >> 1
}

slow!(s_jump_if, |cx| {
    let t = truth(cx, cx.get(cx.a()));
    go(cx, if t as u64 == cx.k() { cx.target() } else { cx.after(1) })
});

h!(
    /// A forward `JumpIfFalse` (`k` 0) or `JumpIfTrue` (`k` 1). A boolean
    /// is its own case.
    h_jump_if,
    |cx| {
        let x = cx.get(cx.a());
        if !is_bool(x) {
            return again!(cx, s_jump_if);
        }
        go(cx, if x & 1 == cx.k() { cx.target() } else { cx.after(1) })
    }
);

slow!(s_jump_if_back, |cx| {
    if truth(cx, cx.get(cx.a())) as u64 == cx.k() {
        return back(cx, cx.target());
    }
    go(cx, cx.after(1))
});

h!(
    /// A backward `JumpIfFalse` / `JumpIfTrue`: a `do … while`'s back-edge
    /// when taken. A boolean is its own case.
    h_jump_if_back,
    |cx| {
        let x = cx.get(cx.a());
        if !is_bool(x) {
            return again!(cx, s_jump_if_back);
        }
        if x & 1 == cx.k() {
            return back(cx, cx.target());
        }
        go(cx, cx.after(1))
    }
);

h!(h_jump, |cx| { go(cx, cx.target()) });

h!(
    /// A backward `Jump`: a loop's back-edge.
    h_jump_back,
    |cx| { back(cx, cx.target()) }
);

slow!(s_add_int_jump, |cx| {
    let Some(r) = add_lit(cx.get(cx.a()), cx.k()) else { return out(cx) };
    cx.put(cx.c(), r);
    back(cx, cx.target())
});

h!(
    /// `FuseAddIntJump`: `c = a + k` on a Number, and the back-edge.
    h_add_int_jump,
    |cx| {
        let Some(r) = add_lit_int(cx.get(cx.a()), cx.k()) else { return again!(cx, s_add_int_jump) };
        cx.put(cx.c(), r);
        back(cx, cx.target())
    }
);

slow!(s_not, |cx| {
    let t = truth(cx, cx.get(cx.a()));
    cx.put(cx.c(), BOOL | !t as u64);
    go(cx, cx.after(1))
});

h!(
    /// `Not`: `c = !a`. A boolean is its own case.
    h_not,
    |cx| {
        let x = cx.get(cx.a());
        if !is_bool(x) {
            return again!(cx, s_not);
        }
        cx.put(cx.c(), x ^ 1);
        go(cx, cx.after(1))
    }
);

slow!(s_not_jump, |cx| {
    let r = !truth(cx, cx.get(cx.a())) as u64;
    cx.put(cx.c(), BOOL | r);
    go(cx, if r == cx.k() { cx.target() } else { cx.after(2) })
});

h!(
    /// `FuseNotJump`: `c = !a`; to the target (forward) when that equals
    /// `k`, else on two positions. A boolean is its own case.
    h_not_jump,
    |cx| {
        let x = cx.get(cx.a());
        if !is_bool(x) {
            return again!(cx, s_not_jump);
        }
        let r = (x ^ 1) & 1;
        cx.put(cx.c(), BOOL | r);
        go(cx, if r == cx.k() { cx.target() } else { cx.after(2) })
    }
);

heavy!(
    /// `LoadConst` of a string: `c` = the constant `k` of the function, as
    /// the dispatch arm resolves it (its first read interns it).
    h_load_const_heavy,
    b_load_const,
    |cx, go, out| {
        let vm = cx.vm();
        let v = vm.resolve_const_slot(vm.lite.loops.func, cx.k() as u32);
        cx.put(cx.c(), v.bits());
        go(cx, cx.after(1))
    }
);

h!(
    /// `LoadConst` of a string: its resolved value from the constant cache
    /// (`resolve_const_string`'s first step), read inline; a constant not
    /// resolved yet, or the cache off, is `h_load_const_heavy`'s.
    h_load_const,
    |cx| {
        let vm = cx.vm();
        if vm.const_string_cache_enabled {
            if let Some(&v) = vm
                .const_string_dense
                .get(vm.lite.loops.func as usize)
                .and_then(|f| f.get(cx.k() as usize))
            {
                if !v.is_undefined() {
                    cx.put(cx.c(), v.bits());
                    return go(cx, cx.after(1));
                }
            }
        }
        again!(cx, h_load_const_heavy)
    }
);

heavy!(
    /// `FuseConstEqJump`: `b` = the constant in the low half of `k`;
    /// `c = (a === b)`; to the target when that equals bit 32 of `k`, else
    /// on three positions.
    h_const_eq_jump,
    b_const_eq_jump,
    |cx, go, out| {
        let vm = cx.vm();
        let k = cx.k();
        let lit = vm.resolve_const_slot(vm.lite.loops.func, k as u32);
        cx.put(cx.b(), lit.bits());
        let r = vm.strict_eq_fast(Value::from_bits(cx.get(cx.a())), lit);
        cx.put(cx.c(), BOOL | r as u64);
        go(cx, if r as u64 == k >> 32 { cx.target() } else { cx.after(3) })
    }
);

h!(
    /// `Neg`: `c = -a` on a Number (an Int32's zero and minimum negate to
    /// doubles).
    h_neg,
    |cx| {
        let x = cx.get(cx.a());
        let vx = Value::from_bits(x);
        let r = if is_int(x) {
            match x as i32 {
                0 => Value::num(-0.0),
                i => match i.checked_neg() {
                    Some(v) => Value::int(v),
                    None => Value::num(-(i as f64)),
                },
            }
        } else if vx.is_double() {
            Value::num(-vx.as_f64())
        } else {
            return out(cx);
        };
        cx.put(cx.c(), r.bits());
        go(cx, cx.after(1))
    }
);

// ── a script's globals ──
// The runner is entered only while every initialized global slot is plain
// (`Vm::loop_globals_plain`), and no handler can change that, so a slot's
// read is its value unless it was never initialized, and a store to an
// initialized slot is the write.

h!(
    /// `LoadGlobal`: `c = globals[d]`.
    h_load_global,
    |cx| {
        let g = *cx.global(cx.d());
        if g == UNINIT {
            return out(cx);
        }
        cx.put(cx.c(), g);
        go(cx, cx.after(1))
    }
);

h!(
    /// `StoreGlobal`, `StoreGlobalStrict`, `StoreGlobalResolved`:
    /// `globals[d] = a`.
    h_store_global,
    |cx| {
        let slot = cx.global(cx.d());
        if *slot == UNINIT {
            return out(cx);
        }
        *slot = cx.get(cx.a());
        go(cx, cx.after(1))
    }
);

h!(
    /// `FuseLoadGlobal2`: `c = globals[d]; a = globals[k]`.
    h_load_global2,
    |cx| {
        let (g, g2) = (*cx.global(cx.d()), *cx.global(cx.k() as u32));
        if g == UNINIT || g2 == UNINIT {
            return out(cx);
        }
        cx.put(cx.c(), g);
        cx.put(cx.a(), g2);
        go(cx, cx.after(2))
    }
);

/// `FuseGlobalCmpJump`: `c = globals[k]`, then on two positions when
/// `a OP c`, else to the target (`a` is not `c`), for two Numbers.
macro_rules! global_cmp_jump {
    ($name:ident, $slow:ident, $cmp:tt) => {
        slow!($slow, |cx| {
            let (x, g) = (cx.get(cx.a()), *cx.global(cx.k() as u32));
            let Some(r) = number_cmp!(x, g, $cmp) else { return out(cx) };
            cx.put(cx.c(), g);
            go(cx, if r { cx.after(2) } else { cx.target() })
        });
        h!($name, |cx| {
            let (x, g) = (cx.get(cx.a()), *cx.global(cx.k() as u32));
            if !ints(x, g) {
                return again!(cx, $slow);
            }
            cx.put(cx.c(), g);
            go(cx, if (x as i32) $cmp (g as i32) { cx.after(2) } else { cx.target() })
        });
    };
}
global_cmp_jump!(h_global_lt_jump, s_global_lt_jump, <);
global_cmp_jump!(h_global_le_jump, s_global_le_jump, <=);

slow!(s_load_global_add_int, |cx| {
    let g = *cx.global(cx.d());
    if g == UNINIT {
        return out(cx);
    }
    cx.put(cx.b(), g);
    // After the write: `a` may be `b`.
    let Some(r) = add_lit(cx.get(cx.a()), cx.k()) else { return out(cx) };
    cx.put(cx.c(), r);
    go(cx, cx.after(2))
});

h!(
    /// `FuseLoadGlobalAddInt`: `b = globals[d]; c = a + k`.
    h_load_global_add_int,
    |cx| {
        let g = *cx.global(cx.d());
        if g == UNINIT {
            return out(cx);
        }
        cx.put(cx.b(), g);
        // After the write: `a` may be `b`.
        let Some(r) = add_lit_int(cx.get(cx.a()), cx.k()) else { return again!(cx, s_load_global_add_int) };
        cx.put(cx.c(), r);
        go(cx, cx.after(2))
    }
);

slow!(s_global_int_lt_jump, |cx| {
    let (g, k) = (*cx.global(cx.c()), cx.k());
    let vg = Value::from_bits(g);
    if !vg.is_double() {
        return out(cx);
    }
    cx.put(cx.a(), g);
    cx.put(cx.b(), k);
    go(cx, if vg.as_f64() < k as i32 as f64 { cx.after(3) } else { cx.target() })
});

h!(
    /// `FuseGlobalIntCmpJump`: `a = globals[c]; b = k`, then on three
    /// positions when `a < k`, else to the target, on a global Number.
    h_global_int_lt_jump,
    |cx| {
        let (g, k) = (*cx.global(cx.c()), cx.k());
        if !is_int(g) {
            return again!(cx, s_global_int_lt_jump);
        }
        cx.put(cx.a(), g);
        cx.put(cx.b(), k);
        go(cx, if (g as i32) < (k as i32) { cx.after(3) } else { cx.target() })
    }
);

slow!(s_global_inc_jump, |cx| {
    let slot = cx.global(cx.a());
    let Some(r) = add_lit(*slot, cx.k()) else { return out(cx) };
    cx.put(cx.c(), r);
    *slot = r;
    back(cx, cx.target())
});

h!(
    /// `FuseGlobalIncJump`: `c = globals[a] + k; globals[a] = c` on a
    /// global Number, and the back-edge.
    h_global_inc_jump,
    |cx| {
        let slot = cx.global(cx.a());
        let Some(r) = add_lit_int(*slot, cx.k()) else { return again!(cx, s_global_inc_jump) };
        cx.put(cx.c(), r);
        *slot = r;
        back(cx, cx.target())
    }
);

slow!(s_global_inc, |cx| {
    let slot = cx.global(cx.a());
    let Some(r) = add_lit(*slot, cx.k()) else { return out(cx) };
    cx.put(cx.c(), r);
    *slot = r;
    go(cx, cx.after(3))
});

h!(
    /// `FuseGlobalInc`: as `h_global_inc_jump`, on three positions.
    h_global_inc,
    |cx| {
        let slot = cx.global(cx.a());
        let Some(r) = add_lit_int(*slot, cx.k()) else { return again!(cx, s_global_inc) };
        cx.put(cx.c(), r);
        *slot = r;
        go(cx, cx.after(3))
    }
);

// ── elements, properties, calls that are leaves ──

heavy!(
    /// `GetIndex`: `c = a[b]`, as the dispatch arm reads it: a numeric
    /// typed array's element and a dense array's present one, its two
    /// leaves, else the general read.
    h_get_index,
    b_get_index,
    |cx, go, out| {
        let vm = cx.vm();
        let (o, k) = (Value::from_bits(cx.get(cx.a())), Value::from_bits(cx.get(cx.b())));
        let leaf = if !o.is_heap() {
            None
        } else {
            let idx = o.heap_index();
            match vm.heap.get(idx) {
                HeapObj::TypedArray {
                    buffer,
                    kind,
                    byte_offset,
                    length,
                } if !native::TA_KINDS[*kind as usize].2 => match array_index(k) {
                    Some(i) => {
                        let view = crate::vm::typedarray::TaInterpNumericView::from_matched_non_bigint(
                            idx,
                            *buffer,
                            *kind,
                            *byte_offset,
                            *length,
                        );
                        vm.ta_interp_numeric_get_view(view, i)
                    }
                    None => None,
                },
                HeapObj::Array(_) => vm.interp_dense_int_get(o, k),
                _ => None,
            }
        };
        match leaf {
            Some(r) => {
                cx.put(cx.c(), r.bits());
                go(cx, cx.after(1))
            }
            None => finish(cx, vm.get_index(o, k)),
        }
    }
);

heavy!(
    /// `SetIndex`: `a[b] = c`, as the dispatch arm stores it: a numeric
    /// typed array's element, its leaf, and the general store, whose first
    /// step for an array is here too (a present element of one with nothing
    /// in a side table, overwritten in place).
    h_set_index,
    b_set_index,
    |cx, go, out| {
        let vm = cx.vm();
        let (o, k, v) = (
            Value::from_bits(cx.get(cx.a())),
            Value::from_bits(cx.get(cx.b())),
            Value::from_bits(cx.get(cx.c())),
        );
        let mut stored = false;
        if o.is_heap() {
            let idx = o.heap_index();
            match vm.heap.get(idx) {
                HeapObj::TypedArray {
                    buffer,
                    kind,
                    byte_offset,
                    length,
                } => {
                    if v.is_number()
                        && !native::TA_KINDS[*kind as usize].2
                        && (vm.immutable_buffers.is_empty() || !vm.immutable_buffers.contains(buffer))
                    {
                        if let Some(i) = array_index(k) {
                            let view = crate::vm::typedarray::TaInterpNumericView::from_matched_non_bigint(
                                idx,
                                *buffer,
                                *kind,
                                *byte_offset,
                                *length,
                            );
                            stored = vm.ta_interp_numeric_set_view(view, i, v);
                        }
                    }
                }
                HeapObj::Array(_) => {
                    if let Some(i) = array_index(k) {
                        if (vm.arr_props.is_empty() || !vm.arr_props.contains_key(&idx))
                            && (vm.arguments_objs.is_empty() || !vm.arguments_objs.contains_key(&idx))
                        {
                            // The store's barrier, as `set_index` raises it first.
                            vm.store_barrier_v(crate::heap::gcoracle::SET_INDEX, o, v);
                            stored = vm.heap.array_overwrite_present(idx, i, v);
                        }
                    }
                }
                _ => {}
            }
        }
        if stored {
            return go(cx, cx.after(1));
        }
        let strict = vm.func(vm.lite.loops.func as usize).is_strict;
        finish_unit(cx, vm.set_index(o, k, v, strict))
    }
);

heavy!(
    /// `GetProp`: `c = a.<name>` by the site's ways as the dispatch arm asks
    /// them (`ic_get_prop_way0`: every layout the site has seen, own or
    /// inherited data, an accessor), then the arm's other steps that run no
    /// code (`Vm::loop_get_prop`). A getter is called as the arm calls it: a
    /// leaf run
    /// without a frame, any other in a frame the runner pushes (`d` the
    /// function, `k` the name's constant). Out of line, as
    /// `h_get_prop_memo`: inlined into the light handler that jumps here,
    /// its call of the body cost that handler saved registers at entry.
    #[inline(never)]
    h_get_prop_heavy,
    b_get_prop,
    |cx, go, out| {
        let vm = cx.vm();
        let o = Value::from_bits(cx.get(cx.a()));
        let ip = cx.ip();
        let Instr::GetProp { name, .. } = *(vm.lite.loops.code as *const Instr).add(ip) else { return out(cx) };
        // The site's ways first, as the dispatch arm asks them: every
        // layout the site has seen, an accessor's way among them.
        #[cfg(all(feature = "jit", not(feature = "safe-sandbox")))]
        let first = vm.ic_get_prop_way0(cx.d(), ip, o, name);
        #[cfg(not(all(feature = "jit", not(feature = "safe-sandbox"))))]
        let first = match vm.ic_get_prop_own_hit(cx.d(), ip, o) {
            Some(v) => GetAct::Value(v),
            None => GetAct::None,
        };
        let act = match first {
            GetAct::None => {
                if !o.is_heap() {
                    return out(cx);
                }
                vm.loop_get_prop(cx.d(), ip, o, name)
            }
            act => act,
        };
        let v = match act {
            GetAct::Value(v) => v,
            GetAct::Accessor { fid, closure, getter } => {
                // An object receiver needs no boxing, even for a sloppy
                // getter.
                let this_is_object = vm.is_object_value(o);
                let base = cx.frame_base();
                match vm.leaf_call(fid, closure, o, this_is_object, base, 0, 0) {
                    Some(v) => v,
                    None => {
                        if !this_is_object {
                            return out(cx);
                        }
                        let desc = vm.lean_call_desc(fid);
                        return match vm.loop_enter(fid, closure, desc, Some(o), base, 0, 0, cx.c() as u16, ip + 1, getter) {
                            Some(first) => go(cx, first),
                            None => out(cx),
                        };
                    }
                }
            }
            GetAct::None => return out(cx),
        };
        cx.put(cx.c(), v.bits());
        go(cx, cx.after(1))
    }
);

h!(
    /// `GetProp`: `c = a.<name>` by the site cache's own-data hit, with no
    /// wrapper around it and nothing out of line before the hand-off
    /// (`ic_get_prop_own_slot`). Anything else is `h_get_prop_memo`'s, a
    /// jump away. Each kind of read has a handler of its own (a site that
    /// reads through its prototype chain starts at `h_get_prop_proto`,
    /// `decode`): asked in one, the three cost it six saved registers and
    /// two spills at its entry, which every read paid (prop_read 9% slower),
    /// and asked in a second, the inherited read and the memo paid each
    /// other's (proto_read 142 ms against 117).
    #[inline(never)]
    h_get_prop,
    |cx| {
        let o = Value::from_bits(cx.get(cx.a()));
        let vm = cx.vm();
        if let Some(v) = vm.ic_get_prop_own_slot(cx.d(), cx.ip(), o) {
            cx.put(cx.c(), v.bits());
            return go(cx, cx.after(1));
        }
        again!(cx, h_get_prop_memo)
    }
);

h!(
    /// `GetProp` at a site whose cache read through the prototype chain
    /// when the runner decoded it: the stamped inherited-data way
    /// (`ic_get_prop_proto_slot`); anything else is `h_get_prop`'s. Out of
    /// line, as the handlers it jumps to.
    #[inline(never)]
    h_get_prop_proto,
    |cx| {
        let o = Value::from_bits(cx.get(cx.a()));
        let vm = cx.vm();
        if let Some(v) = vm.ic_get_prop_proto_slot(cx.d(), cx.ip(), o) {
            cx.put(cx.c(), v.bits());
            return go(cx, cx.after(1));
        }
        again!(cx, h_get_prop)
    }
);

h!(
    /// `GetProp` past the own-data hit: for a name of up to 8 bytes,
    /// another layout's own data property from the (shape, key) memo.
    /// Anything else (an inherited property, an accessor, a receiver with no
    /// shape) is `h_get_prop_heavy`'s. Out of line: a handler only one
    /// other jumps to is inlined into it, prologue and all.
    #[inline(never)]
    h_get_prop_memo,
    |cx| {
        let o = Value::from_bits(cx.get(cx.a()));
        let vm = cx.vm();
        // Another layout's own data property, by the (shape, key) memo.
        if cx.b() & GETPROP_WORDS as u32 != 0 {
            if let Some(v) = vm.ic_get_prop_mega_short(cx.d(), cx.ip(), o, cx.k(), cx.b() & 0xFF) {
                cx.put(cx.c(), v.bits());
                return go(cx, cx.after(1));
            }
        }
        again!(cx, h_get_prop_heavy)
    }
);

/// In `b` of a `GetProp` position: `k` holds the name's key word for the
/// (shape, key) memo, and the low byte its length.
const GETPROP_WORDS: u16 = 0x100;

heavy!(
    /// `SetProp`: `a.<name> = b` by the dispatch arm's steps (a private
    /// name is never asked here: `decode`). The site's ways
    /// (`ic_set_prop_way0`: an own slot in any layout the site has seen, an
    /// append, a setter); a Proxy's trap; a class's or a function's own
    /// data property; the
    /// cache's other writes (an append), or the setter it names, called in
    /// a frame the runner pushes; then the general store. `c` the site's
    /// strictness, `d` the function, `k` the name's constant.
    h_set_prop,
    b_set_prop,
    |cx, go, out| {
        let vm = cx.vm();
        let (o, v) = (Value::from_bits(cx.get(cx.a())), Value::from_bits(cx.get(cx.b())));
        let ip = cx.ip();
        // A write to the global object can change how a global slot is
        // reached, which the runner asked once, at its entry.
        if o.is_heap() && o.heap_index() == vm.global_this {
            return out(cx);
        }
        // The site's ways first, as the dispatch arm asks them: an own
        // slot of any layout the site has seen, an append, a setter.
        #[cfg(all(feature = "jit", not(feature = "safe-sandbox")))]
        match vm.ic_set_prop_way0(cx.d(), ip, o, v, cx.k() as u32) {
            SetAct::Done => return go(cx, cx.after(1)),
            SetAct::Setter { fid, closure, setter } => {
                // A shaped receiver is an object: no boxing.
                let desc = vm.lean_call_desc(fid);
                let (base, val) = (cx.frame_base(), cx.b() as u16);
                return match vm.loop_enter(fid, closure, desc, Some(o), base, val, 1, super::ic::RET_DISCARD, ip + 1, setter) {
                    Some(first) => go(cx, first),
                    None => out(cx),
                };
            }
            SetAct::None => {}
        }
        #[cfg(not(all(feature = "jit", not(feature = "safe-sandbox"))))]
        if vm.ic_set_prop_own_hit(cx.d(), ip, o, v) {
            return go(cx, cx.after(1));
        }
        let key: &str = vm.func(cx.d() as usize).string_constants[cx.k() as usize].as_str();
        let strict = vm.func(cx.d() as usize).is_strict || cx.c() != 0;
        if o.is_heap() {
            // 0 an ordinary object, 1 a Proxy, 2 anything else.
            let kind = match vm.heap.get(o.heap_index()) {
                HeapObj::Object(_) => 0,
                HeapObj::Proxy { .. } => 1,
                _ => 2,
            };
            if kind == 1 {
                return finish_unit(cx, vm.set_prop_proxy(o, key, v, strict).map(|_| ()));
            }
            if kind == 2 && (vm.class_static_data_write(o, key, v) || vm.fn_own_data_write(o, key, v)) {
                return go(cx, cx.after(1));
            }
        }
        match vm.ic_set_prop(cx.d(), ip, o, key, v) {
            SetAct::Done => go(cx, cx.after(1)),
            SetAct::Setter { fid, closure, setter } => {
                // The lean push, as for a getter: an object receiver needs
                // no boxing. The value is the setter's one argument.
                if !vm.is_object_value(o) {
                    return out(cx);
                }
                let desc = vm.lean_call_desc(fid);
                let (base, val) = (cx.frame_base(), cx.b() as u16);
                match vm.loop_enter(fid, closure, desc, Some(o), base, val, 1, super::ic::RET_DISCARD, ip + 1, setter) {
                    Some(first) => go(cx, first),
                    None => out(cx),
                }
            }
            SetAct::None => finish_unit(cx, vm.set_prop(o, key, v, strict).map(|_| ())),
        }
    }
);

heavy!(
    /// `GetProp` of `length`: the site cache's own-data hit, then the
    /// internal length of a string, an array or a typed array.
    h_get_length,
    b_get_length,
    |cx, go, out| {
        let vm = cx.vm();
        let o = Value::from_bits(cx.get(cx.a()));
        let v = match vm.ic_get_prop_own_hit(cx.d(), cx.ip(), o) {
            Some(v) => v,
            None => {
                if !o.is_heap() {
                    return out(cx);
                }
                match vm.length_fast(o) {
                    Some(v) => v,
                    None => return out(cx),
                }
            }
        };
        cx.put(cx.c(), v.bits());
        go(cx, cx.after(1))
    }
);

heavy!(
    /// `GetPropDv`: the site cache's own-data hit, then a pristine
    /// DataView's accessor read (`k` the accessor's entry).
    h_get_prop_dv,
    b_get_prop_dv,
    |cx, go, out| {
        let vm = cx.vm();
        let o = Value::from_bits(cx.get(cx.a()));
        let v = match vm.ic_get_prop_own_hit(cx.d(), cx.ip(), o) {
            Some(v) => v.bits(),
            None => {
                if !o.is_heap() {
                    return out(cx);
                }
                match vm.dataview_proto_read_slot(o.heap_index(), cx.k() as usize) {
                    Some(bits) => bits,
                    None => return out(cx),
                }
            }
        };
        cx.put(cx.c(), v);
        go(cx, cx.after(1))
    }
);

heavy!(
    /// `CallMethodDv`: `c = a.<accessor>(args)` on a pristine DataView (`b`
    /// the first argument's register, `d` the count, `k` the entry).
    h_call_dv,
    b_call_dv,
    |cx, go, out| {
        let vm = cx.vm();
        let bits = vm.dataview_site_call(
            Value::from_bits(cx.get(cx.a())),
            cx.k() as usize,
            cx.frame_base(),
            cx.b() as u16,
            cx.d() as u16,
        );
        if bits == crate::vm::typedarray::DV_SITE_DECLINE {
            return out(cx);
        }
        cx.put(cx.c(), bits);
        go(cx, cx.after(1))
    }
);

heavy!(
    /// `CallWithThisDv`: `c = callee.call(this, args)` when the captured
    /// callee (`a`) is a numeric DataView accessor and `this` (`b`) a
    /// DataView (`d` the first argument's register, `k` the count).
    h_call_this_dv,
    b_call_this_dv,
    |cx, go, out| {
        let vm = cx.vm();
        let bits = vm.dataview_site_captured(
            Value::from_bits(cx.get(cx.b())),
            Value::from_bits(cx.get(cx.a())),
            cx.frame_base(),
            cx.d() as u16,
            cx.k() as u16,
        );
        if bits == crate::vm::typedarray::DV_SITE_DECLINE {
            return out(cx);
        }
        cx.put(cx.c(), bits);
        go(cx, cx.after(1))
    }
);

heavy!(
    /// `MathOp` on Numbers while `Math` and the method are the builtins:
    /// the leaf the frameless callee runs (`lite::leaf_run`).
    h_math,
    b_math,
    |cx, go, out| {
        let vm = cx.vm();
        let Instr::MathOp {
            dst,
            op,
            callee,
            this_v,
            arg_base,
            argc,
        } = *(vm.lite.loops.code as *const Instr).add(cx.ip())
        else {
            return out(cx);
        };
        let intrinsic = if callee == crate::bytecode::NO_REG {
            vm.math_bare_is_intrinsic(op, this_v as u32)
        } else {
            vm.math_method_is_intrinsic(
                native::MATH_METHOD_BASE + op as u16,
                Value::from_bits(cx.get(callee as u32)),
                Value::from_bits(cx.get(this_v as u32)),
            )
        };
        let args = std::slice::from_raw_parts(cx.regs.add(arg_base as usize) as *const Value, argc as usize);
        // Numbers only: anything else's ToNumber can run code.
        if !intrinsic || !args.iter().all(|v| v.is_number()) {
            return out(cx);
        }
        let v = match (op, args) {
            (crate::bytecode::MathFn::Imul, [a, b, ..]) if a.is_int() && b.is_int() => {
                Value::int(a.as_int().wrapping_mul(b.as_int()))
            }
            _ => match vm.eval_math_args(op, args) {
                Ok(x) => Value::num(x),
                Err(_) => return out(cx),
            },
        };
        cx.put(dst as u32, v.bits());
        go(cx, cx.after(1))
    }
);

heavy!(
    /// `FuseIntSwitch` / `FuseStrSwitch`: the dispatch loops' table lookup,
    /// which answers the next position (`d` the function).
    h_switch,
    b_switch,
    |cx, go, out| {
        let vm = cx.vm();
        let ip = cx.ip();
        let running = vm.func(cx.d() as usize);
        let next = match *(vm.lite.loops.code as *const Instr).add(ip) {
            Instr::FuseIntSwitch { ldst, a, dst, n } => vm.int_switch(running, cx.frame_base(), ip, ldst, a, dst, n),
            Instr::FuseStrSwitch { ldst, a, dst, n } => {
                vm.str_switch(running, cx.d(), cx.frame_base(), ip, ldst, a, dst, n)
            }
            _ => return out(cx),
        };
        go(cx, (vm.lite.loops.ops as *const Op).add(next))
    }
);

/// `k` of a `CallMethod` position: its name is `push` and it has one
/// argument (`h_call_method`'s array append), `charCodeAt` and one, or `pop`
/// and none.
const METHOD_PUSH: u64 = 1;
const METHOD_CHAR_CODE_AT: u64 = 2;
const METHOD_POP: u64 = 3;

heavy!(
    /// `CallMethod`: `c = a.<name>(args)` by the dispatch arm's steps. An
    /// ordinary object's or class instance's method, by the lite loop's
    /// entry for the site, then the method cache's ways and the cache
    /// (`Vm::loop_method_other`):
    /// a leaf run without a frame (`Vm::leaf_call`), any other in a frame
    /// the runner pushes and goes on in (`Vm::loop_enter`); an own native
    /// method. `push` with one argument and `pop` with none on an array;
    /// `charCodeAt` with one on a flat string; any other builtin's method on
    /// a string, an array, a Map or Set or a number
    /// (`Vm::loop_method_builtin`). `b` the first argument's register, `d`
    /// the function, `k` which name.
    h_call_method,
    b_call_method,
    |cx, go, out| {
        let vm = cx.vm();
        let recv = Value::from_bits(cx.get(cx.a()));
        let ip = cx.ip();
        let at = &*(vm.lite.loops.code as *const Instr).add(ip);
        let Instr::CallMethod { name, argc, .. } = *at else { return out(cx) };
        let (base, arg_base) = (cx.frame_base(), cx.b() as u16);
        let v = if !recv.is_heap() {
            None
        } else {
            match vm.heap.get(recv.heap_index()) {
                HeapObj::Object(_) => {
                    // The lite loop's entry (the one layout a site
                    // usually has), then every other layout the site has
                    // seen (`Vm::loop_method_other`).
                    let hit = match vm.lite_method_entry(at, cx.d(), ip, recv) {
                        Some(hit) => Some(hit),
                        None => vm.loop_method_other(at, cx.d(), ip, recv, name),
                    };
                    let Some((fid, closure, callee, desc)) = hit else {
                        // An own native method (`Reflect.get(t, k)`).
                        let key: &str = vm.func(cx.d() as usize).string_constants[name as usize].as_str();
                        return match vm.ic_call_native(cx.d(), ip, recv, key) {
                            Some(id) => {
                                let r = vm.with_argv(base, arg_base, argc, |vm, argv| vm.call_native(id, recv, argv));
                                finish(cx, r)
                            }
                            None => out(cx),
                        };
                    };
                    match vm.leaf_call(fid, closure, recv, true, base, arg_base, argc) {
                        Some(v) => Some(v),
                        None => {
                            let dst = cx.c() as u16;
                            return match vm.loop_enter(fid, closure, desc, Some(recv), base, arg_base, argc, dst, ip + 1, callee) {
                                Some(first) => go(cx, first),
                                None => out(cx),
                            };
                        }
                    }
                }
                HeapObj::Array(_) if cx.k() == METHOD_PUSH => {
                    vm.interp_array_push_fast(recv, Value::from_bits(cx.get(cx.b())))
                }
                HeapObj::Array(_) if cx.k() == METHOD_POP => vm.interp_array_pop_fast(recv),
                HeapObj::Str(_) if cx.k() == METHOD_CHAR_CODE_AT => {
                    vm.interp_char_code_at_fast(recv, Value::from_bits(cx.get(cx.b())))
                }
                // A Map's or WeakMap's `get`, `set`, `has` and `delete` by
                // the dispatch arm's lane, reached from here rather than
                // through `Vm::loop_method_builtin`: nothing in it runs
                // program code, so the registers stand and only a collection
                // can be due (a `set` that grew the map). What the lane
                // declines goes on to the builtin dispatcher as before.
                HeapObj::Map { .. } | HeapObj::WeakMap { .. } if argc == 1 || argc == 2 => {
                    let key: &str = vm.func(cx.d() as usize).string_constants[name as usize].as_str();
                    match vm.interp_map_method_fast(recv, key, base, arg_base, argc) {
                        Ok(Some(v)) => {
                            cx.put(cx.c(), v.bits());
                            return after_alloc(cx, cx.after(1));
                        }
                        Ok(None) => None,
                        Err(e) => return raise(cx, e),
                    }
                }
                _ => None,
            }
        };
        match v {
            Some(v) => {
                cx.put(cx.c(), v.bits());
                go(cx, cx.after(1))
            }
            None => match vm.loop_method_builtin(cx.d(), ip, recv, name, base, arg_base, argc) {
                Ok(None) => {
                    let (cx, _) = settle(cx);
                    out(cx)
                }
                Ok(Some(v)) => finish(cx, Ok(v)),
                Err(e) => finish(cx, Err(e)),
            },
        }
    }
);

heavy!(
    /// `Call`: `c = a(args)` when the callee is a plain function: a leaf
    /// run without a frame, any other in a frame the runner pushes and goes
    /// on in (`b` the first argument's register, `d` the function, `k` the
    /// argument count).
    h_call,
    b_call,
    |cx, go, out| {
        let vm = cx.vm();
        let callee = Value::from_bits(cx.get(cx.a()));
        let ip = cx.ip();
        let Some((fid, closure, desc)) = vm.loop_resolve(cx.d(), ip, callee) else { return out(cx) };
        let (base, arg_base, argc) = (cx.frame_base(), cx.b() as u16, cx.k() as u16);
        if let Some(v) = vm.leaf_call(fid, closure, Value::UNDEFINED, false, base, arg_base, argc) {
            cx.put(cx.c(), v.bits());
            return go(cx, cx.after(1));
        }
        match vm.loop_enter(fid, closure, desc, None, base, arg_base, argc, cx.c() as u16, ip + 1, callee) {
            Some(first) => go(cx, first),
            None => out(cx),
        }
    }
);

heavy!(
    /// `Return` from a frame this entry pushed: the value in `a` to the
    /// caller, and on at the position after its call (`Vm::loop_return`).
    /// The return of the frame the runner entered in is the dispatch
    /// loop's.
    h_return,
    b_return,
    |cx, go, out| {
        match cx.vm().loop_return(Value::from_bits(cx.get(cx.a()))) {
            Some(next) => go(cx, next),
            None => out(cx),
        }
    }
);

heavy!(
    /// `FuseAddReturn`: `c = a + b` on two Numbers, then `Return` of `d`
    /// (`h_return`). Nothing is written unless the return is the runner's.
    h_add_return,
    b_add_return,
    |cx, go, out| {
        let vm = cx.vm();
        let (x, y) = (Value::from_bits(cx.get(cx.a())), Value::from_bits(cx.get(cx.b())));
        if !vm.loop_returns() || !(x.is_number() && y.is_number()) {
            return out(cx);
        }
        let r = if x.is_int() && y.is_int() {
            match x.as_int().checked_add(y.as_int()) {
                Some(v) => Value::int(v),
                None => Value::num(x.as_int() as f64 + y.as_int() as f64),
            }
        } else {
            Value::num(x.as_f64() + y.as_f64())
        };
        cx.put(cx.c(), r.bits());
        let v = Value::from_bits(cx.get(cx.d()));
        go(cx, vm.loop_return_now(v))
    }
);

heavy!(
    /// `CallWithThis`: `c = a.call(b, args)` in a member call's spec order,
    /// by the dispatch arm's steps. A plain function whose `this` needs no
    /// boxing (a strict callee, or an object): a leaf run without a frame,
    /// any other in a frame the runner pushes. A native callee
    /// (`s.charCodeAt(g)`, `map.get(g)`): `Vm::loop_call_this_native`. `d`
    /// the first argument's register, `k` the count under the site's name.
    h_call_this,
    b_call_this,
    |cx, go, out| {
        let vm = cx.vm();
        let (callee, this_val) = (Value::from_bits(cx.get(cx.a())), Value::from_bits(cx.get(cx.b())));
        let (base, arg_base, argc) = (cx.frame_base(), cx.d() as u16, cx.k() as u16);
        let Some((fid, closure)) = vm.ic_plain_fn(callee) else {
            let name = (cx.k() >> 32) as u32;
            // The two leaves first: nothing of theirs runs code.
            if let Some(v) = vm.captured_leaf_call(this_val, callee, base, arg_base, argc) {
                cx.put(cx.c(), v.bits());
                return go(cx, cx.after(1));
            }
            return match vm.loop_call_this_native(vm.lite.loops.func, name, callee, this_val, base, arg_base, argc) {
                Ok(None) => {
                    let (cx, _) = settle(cx);
                    out(cx)
                }
                Ok(Some(v)) => finish(cx, Ok(v)),
                Err(e) => finish(cx, Err(e)),
            };
        };
        let this_ok = vm.func(fid as usize).is_strict || vm.is_object_value(this_val);
        if let Some(v) = vm.leaf_call(fid, closure, this_val, this_ok, base, arg_base, argc) {
            cx.put(cx.c(), v.bits());
            return go(cx, cx.after(1));
        }
        if !this_ok {
            return out(cx);
        }
        let desc = vm.lean_call_desc(fid);
        match vm.loop_enter(fid, closure, desc, Some(this_val), base, arg_base, argc, cx.c() as u16, cx.ip() + 1, callee) {
            Some(first) => go(cx, first),
            None => out(cx),
        }
    }
);

heavy!(
    /// `ReturnUndefined`: as `h_return`.
    h_return_undefined,
    b_return_undefined,
    |cx, go, out| {
        match cx.vm().loop_return(Value::UNDEFINED) {
            Some(next) => go(cx, next),
            None => out(cx),
        }
    }
);

/// The running closure's upvalue list (`LoopTier::upv`), taken now if this
/// frame has not read a captured variable yet. `false` when the frame has
/// no closure. It takes the VM's address, not a `Cx`: a `Cx` passed by
/// value is a copy on the caller's stack whose address the call takes, and a
/// light handler with such a copy alive cannot hand off with a jump.
#[cold]
#[inline(never)]
unsafe fn upval_list(vm: *mut c_void) -> bool {
    let vm = &mut *(vm as *mut Vm);
    let closure = vm.lite.loops.closure;
    if closure == NO_CLOSURE || closure as usize >= vm.heap.len() {
        return false;
    }
    match vm.heap.get(closure) {
        HeapObj::Closure { upvalues, .. } => {
            vm.lite.loops.upv = upvalues.as_ptr() as usize;
            vm.lite.loops.upv_len = upvalues.len();
            true
        }
        _ => false,
    }
}

/// The cell of the running closure's captured variable `idx`, when it holds
/// a value (a binding still in its dead zone is the dispatch arm's error).
#[inline(always)]
unsafe fn upval(cx: Cx, idx: u32) -> Option<(u32, Value)> {
    let vm = cx.vm();
    if vm.lite.loops.upv == 0 && !upval_list(cx.vm) {
        return None;
    }
    let t = &vm.lite.loops;
    if idx as usize >= t.upv_len {
        return None;
    }
    // SAFETY: `upv` is the running closure's list (`upval_list`), dropped at
    // every frame change, and `idx` is in its bounds.
    let cell = *(t.upv as *const u32).add(idx as usize);
    let v = vm.heap.cell_get(cell);
    (!v.is_uninitialized()).then_some((cell, v))
}

h!(
    /// `UpvalGet`: `c` = the closure's captured variable `d`.
    h_upval_get,
    |cx| {
        let Some((_, v)) = upval(cx, cx.d()) else { return out(cx) };
        cx.put(cx.c(), v.bits());
        go(cx, cx.after(1))
    }
);

h!(
    /// `FuseUpvalGetMove`: `h_upval_get`, then `b = a`.
    h_upval_get_move,
    |cx| {
        let Some((_, v)) = upval(cx, cx.d()) else { return out(cx) };
        cx.put(cx.c(), v.bits());
        cx.put(cx.b(), cx.get(cx.a()));
        go(cx, cx.after(2))
    }
);

heavy!(
    /// `UpvalSet`: the closure's captured variable `d` = `a`, unless it is
    /// a constant or a function's own name (the dispatch arm's).
    h_upval_set,
    b_upval_set,
    |cx, go, out| {
        let vm = cx.vm();
        let Some((cell, _)) = upval(cx, cx.d()) else { return out(cx) };
        if (!vm.const_cells.is_empty() && vm.const_cells.contains(&cell))
            || (!vm.fn_name_cells.is_empty() && vm.fn_name_cells.contains(&cell))
        {
            return out(cx);
        }
        vm.heap.upval_cell_set(cell, Value::from_bits(cx.get(cx.a())));
        go(cx, cx.after(1))
    }
);

h!(
    /// `CellGet`: `c` = the variable in the cell register `a` holds.
    h_cell_get,
    |cx| {
        let v = cx.vm().heap.cell_get(Value::from_bits(cx.get(cx.a())).heap_index());
        if v.is_uninitialized() {
            return out(cx);
        }
        cx.put(cx.c(), v.bits());
        go(cx, cx.after(1))
    }
);

heavy!(
    /// `CellSet`: the variable in the cell register `a` holds = `b`.
    h_cell_set,
    b_cell_set,
    |cx, go, out| {
        let cell = Value::from_bits(cx.get(cx.a())).heap_index();
        cx.vm().heap.cell_set(cell, Value::from_bits(cx.get(cx.b())));
        go(cx, cx.after(1))
    }
);

heavy!(
    /// `IterNext`, as the dispatch arm steps it. Over an array the
    /// iteration reads directly (its first case): the element at the cursor
    /// `b` of the array `a` into `c` with `d` false, or `d` true past the
    /// last. A generator the arm runs as a frame of its own loop is the
    /// arm's; every other iterator takes the arm's general step.
    h_iter_next,
    b_iter_next,
    |cx, go, out| {
        let vm = cx.vm();
        let it = Value::from_bits(cx.get(cx.a()));
        if !it.is_heap() {
            return out(cx);
        }
        let idx = it.heap_index();
        if (vm.array_js_len.is_empty() || !vm.array_js_len.contains_key(&idx)) && !vm.array_elements_overlaid(idx) {
            let cur = array_index(Value::from_bits(cx.get(cx.b()))).unwrap_or(0);
            if let HeapObj::Array(items) = vm.heap.get(idx) {
                match items.get(cur) {
                    Some(v) if !v.is_hole() => {
                        let v = *v;
                        cx.put(cx.c(), v.bits());
                        cx.put(cx.d(), Value::bool(false).bits());
                        cx.put(cx.b(), Value::int((cur + 1) as i32).bits());
                        return go(cx, cx.after(1));
                    }
                    Some(_) => {}
                    None => {
                        cx.put(cx.d(), Value::bool(true).bits());
                        return go(cx, cx.after(1));
                    }
                }
            }
        }
        let Instr::IterNext { next, .. } = *(vm.lite.loops.code as *const Instr).add(cx.ip()) else {
            return out(cx);
        };
        let base = cx.frame_base();
        if vm.iter_generator_direct(base, it, next) {
            return out(cx);
        }
        let r = vm.iter_next_step(base, it, cx.c() as u16, cx.d() as u16, cx.b() as u16, next);
        finish_unit(cx, r)
    }
);

heavy!(
    /// `NewArray`: `c` = an array of the `d` registers from `a`.
    h_new_array,
    b_new_array,
    |cx, go, out| {
        let vm = cx.vm();
        let (first, n) = (cx.a(), cx.d());
        let mut items = vm.heap.take_arr_buf(n as usize);
        for i in 0..n {
            items.push(Value::from_bits(cx.get(first + i)));
        }
        let v = Value::heap(vm.heap.alloc(HeapObj::Array(items)));
        // ArrayCreate uses the current realm's %Array.prototype%.
        vm.realm_born(v.heap_index(), vm.arr_proto);
        cx.put(cx.c(), v.bits());
        after_alloc(cx, cx.after(1))
    }
);

heavy!(
    /// `NewObject`: `c` = an empty object with room for `k` properties.
    h_new_object,
    b_new_object,
    |cx, go, out| {
        let vm = cx.vm();
        let v = Value::heap(vm.heap.alloc(HeapObj::Object(Box::new(ObjMap::with_capacity(cx.k() as usize)))));
        vm.realm_born(v.heap_index(), vm.obj_proto);
        cx.put(cx.c(), v.bits());
        after_alloc(cx, cx.after(1))
    }
);

heavy!(
    /// `FinalizeObject`: `c` = the object literal of the site's key plan
    /// (`k`) over the `b` values from `a`, as the dispatch arm builds it; a
    /// plan it would refuse is its to refuse (`d` the function).
    h_finalize_object,
    b_finalize_object,
    |cx, go, out| {
        let vm = cx.vm();
        let (val_base, count, plan_idx) = (cx.a(), cx.b() as usize, cx.k() as u16);
        let Some(plan) = vm.func(cx.d() as usize).static_key_plans.get(plan_idx as usize) else {
            return out(cx);
        };
        if !plan.runtime_valid()
            || plan.len() > crate::bytecode::FINALIZE_STAGE_SLOTS
            || plan.is_empty()
            || plan.len() != count
            || val_base as usize + count > u16::MAX as usize
        {
            return out(cx);
        }
        let shape = vm.finalize_shape(cx.d(), plan_idx, plan);
        let mut buf = [Value::UNDEFINED; crate::bytecode::FINALIZE_STAGE_SLOTS];
        for (i, slot) in buf[..count].iter_mut().enumerate() {
            *slot = Value::from_bits(cx.get(val_base + i as u32));
        }
        let v = Value::heap(vm.alloc_finalized_site(plan, &buf[..count], shape));
        vm.realm_born(v.heap_index(), vm.obj_proto);
        cx.put(cx.c(), v.bits());
        after_alloc(cx, cx.after(1))
    }
);

heavy!(
    /// `MakeClosure` and `MakeArrow`: `c` = a closure of function `d` over
    /// the frame's cells (`k` set: an arrow, its `this` in `a`).
    h_make_closure,
    b_make_closure,
    |cx, go, out| {
        let vm = cx.vm();
        let this_reg = (cx.k() != 0).then_some(cx.a() as u16);
        let v = vm.make_closure_in_frame(vm.frames.len() - 1, cx.frame_base(), cx.d(), this_reg);
        cx.put(cx.c(), v.bits());
        after_alloc(cx, cx.after(1))
    }
);

heavy!(
    /// `MakeCell`: register `c` = a cell holding what it held; `MakeCellTdz`
    /// (`k` set): a cell in its dead zone.
    h_make_cell,
    b_make_cell,
    |cx, go, out| {
        let init = if cx.k() != 0 {
            Value::UNINITIALIZED
        } else {
            Value::from_bits(cx.get(cx.c()))
        };
        let cell = cx.vm().heap.alloc_cell(init);
        cx.put(cx.c(), Value::heap(cell).bits());
        after_alloc(cx, cx.after(1))
    }
);

heavy!(
    /// `PushHandler` and `PushFinally`: the frame's handler stack takes the
    /// instruction's `catch` or `finally`.
    h_push_handler,
    b_push_handler,
    |cx, go, out| {
        let vm = cx.vm();
        let handler = match *(vm.lite.loops.code as *const Instr).add(cx.ip()) {
            Instr::PushHandler { catch_target, catch_reg } => crate::heap::Handler::Catch {
                target: catch_target,
                reg: catch_reg,
            },
            Instr::PushFinally { target, kind_reg, val_reg } => crate::heap::Handler::Finally {
                target,
                kind_reg,
                val_reg,
            },
            _ => return out(cx),
        };
        let top = vm.frames.len() - 1;
        // A recycled buffer for the frame's first push.
        if vm.frames[top].handlers.capacity() == 0 {
            vm.frames[top].handlers = super::handler_pool::take(&mut vm.handler_pool);
        }
        vm.frames[top].handlers.push(handler);
        go(cx, cx.after(1))
    }
);

heavy!(
    /// `PopHandler`, `PopFinally` and `FusePopHandlerJump`: the frame's
    /// handler stack gives its last back, and on at `d`.
    h_pop_handler,
    b_pop_handler,
    |cx, go, out| {
        let vm = cx.vm();
        let top = vm.frames.len() - 1;
        vm.frames[top].handlers.pop();
        go(cx, cx.target())
    }
);

h!(
    /// `EndFinally` after a `finally` entered in the ordinary course (its
    /// kind register `a` holds 0): on. Any other completion is the dispatch
    /// arm's to resume.
    h_end_finally,
    |cx| {
        let kind = cx.get(cx.a());
        if !is_int(kind) || kind as u32 & 3 != 0 {
            return out(cx);
        }
        go(cx, cx.after(1))
    }
);

heavy!(
    /// `FuseTypeOfJump`: `c = (typeof a is the type k names)`, negated as
    /// `k` says, then a branch on it to `d`, or on two positions.
    h_typeof_jump,
    b_typeof_jump,
    |cx, go, out| {
        let k = cx.k();
        let m = (cx.vm().type_code(Value::from_bits(cx.get(cx.a()))) == k as u8) != (k >> 8 & 1 != 0);
        cx.put(cx.c(), Value::bool(m).bits());
        if m == (k >> 9 & 1 != 0) {
            go(cx, cx.target())
        } else {
            go(cx, cx.after(2))
        }
    }
);

slow!(s_null_jump, |cx| {
    // A heap value loosely equals `null` only as `document.all` does.
    let v = Value::from_bits(cx.get(cx.a()));
    let k = cx.k();
    let r = cx.vm().is_htmldda_index(v.heap_index()) != (k & 1 != 0);
    cx.put(cx.c(), Value::bool(r).bits());
    if r == (k & 2 != 0) {
        go(cx, cx.target())
    } else {
        go(cx, cx.after(3))
    }
});

h!(
    /// `FuseNullJump`: `b = null; c = (a == null)`, negated as `k` says,
    /// then a branch on it to `d`, or on three positions.
    h_null_jump,
    |cx| {
        cx.put(cx.b(), Value::NULL.bits());
        let v = Value::from_bits(cx.get(cx.a()));
        if v.is_heap() {
            return again!(cx, s_null_jump);
        }
        let k = cx.k();
        let r = v.is_nullish() != (k & 1 != 0);
        cx.put(cx.c(), Value::bool(r).bits());
        if r == (k & 2 != 0) {
            go(cx, cx.target())
        } else {
            go(cx, cx.after(3))
        }
    }
);

heavy!(
    /// `LooseEq` and `LooseNe` on operands that are not two Int32s: `null`
    /// and `undefined` by `loose_eq_nullish`, else the general comparison.
    h_loose_eq_any,
    b_loose_eq_any,
    |cx, go, out| {
        let vm = cx.vm();
        let (x, y) = (Value::from_bits(cx.get(cx.a())), Value::from_bits(cx.get(cx.b())));
        let ne = cx.k() != 0;
        match vm.loose_eq_nullish(x, y) {
            Some(r) => {
                cx.put(cx.c(), Value::bool(r != ne).bits());
                go(cx, cx.after(1))
            }
            None => finish(cx, vm.loose_eq(x, y).map(|r| Value::bool(r != ne))),
        }
    }
);

h!(
    /// `LooseEq` (`k` 0) and `LooseNe` (`k` 1): `c = (a == b)` on two
    /// Int32s, as `Eq` decides them.
    h_loose_eq,
    |cx| {
        let (x, y) = (cx.get(cx.a()), cx.get(cx.b()));
        if !ints(x, y) {
            return again!(cx, h_loose_eq_any);
        }
        cx.put(cx.c(), Value::bool((x == y) != (cx.k() != 0)).bits());
        go(cx, cx.after(1))
    }
);

heavy!(
    /// `StrAppendIndex`: `dst = a + obj[key]`, as the dispatch arm makes it
    /// (its registers are the instruction's).
    h_str_append_index,
    b_str_append_index,
    |cx, go, out| {
        let vm = cx.vm();
        let Instr::StrAppendIndex {
            dst,
            a,
            obj,
            key,
            scratch,
        } = *(vm.lite.loops.code as *const Instr).add(cx.ip())
        else {
            return out(cx);
        };
        let base = cx.frame_base();
        let (av, ov, kv) = (vm.get(base, a), vm.get(base, obj), vm.get(base, key));
        let r = match vm.str_append_index_ascii_fast(av, ov, kv) {
            Some(r) => Ok(r),
            // The element's read may run code and move the register file:
            // the registers by their indices from here.
            None => match vm.get_index(ov, kv) {
                Ok(bv) => {
                    vm.set(base, scratch, bv);
                    let (av, bv) = (vm.get(base, a), vm.get(base, scratch));
                    match vm.str_append_inplace(av, bv) {
                        Some(r) => Ok(r),
                        None => vm.add_values(av, bv),
                    }
                }
                Err(e) => Err(e),
            },
        };
        let (cx, on) = settle(cx);
        match r {
            Ok(v) => {
                cx.put(dst as u32, v.bits());
                then(cx, cx.after(1), on)
            }
            Err(e) => raise(cx, e),
        }
    }
);

heavy!(
    /// `GetIterator`: `c` = the iterator of `a`.
    h_get_iterator,
    b_get_iterator,
    |cx, go, out| {
        let v = Value::from_bits(cx.get(cx.a()));
        finish(cx, cx.vm().get_iterator(v))
    }
);

/// The position of instruction `ip` of function `func_id`: its handler and
/// operands. An instruction with no handler, or in a shape its handler does
/// not take (two operands in one register where the handler reads after it
/// writes, a global slot past the `nglobals` there are), leaves.
fn decode(f: &FuncProto, code: &[Instr], ip: usize, func_id: u32, nglobals: usize, proto_site: &dyn Fn(usize) -> bool) -> Op {
    let op = |h: Handler, a: u16, b: u16, c: u16, d: u32, k: u64| Op {
        h,
        a: a as u32,
        b: b as u32,
        c: c as u32,
        d,
        k,
    };
    // A jump's target as the byte offset from this position.
    let at = |target: u32| ((target as i64 - ip as i64) * std::mem::size_of::<Op>() as i64) as i32 as u32;
    let lit = |v: i32| INT | v as u32 as u64;
    let leave = op(h_out, 0, 0, 0, 0, 0);
    let fits = |target: u32| (target as usize) < code.len() && code.len() < (i32::MAX as usize) / 64;
    let global = |idx: u32| (idx as usize) < nglobals;
    let private = |name: u32| f.string_constants.get(name as usize).is_none_or(|n| is_private_key(n));
    let named = |name: u32, text: &str| f.string_constants.get(name as usize).is_some_and(|n| n == text);
    match code[ip] {
        Instr::LoadInt { dst, val } => op(h_load, 0, 0, dst, 0, lit(val)),
        Instr::LoadBool { dst, val } => op(h_load, 0, 0, dst, 0, Value::bool(val).bits()),
        Instr::LoadNull { dst } => op(h_load, 0, 0, dst, 0, Value::NULL.bits()),
        Instr::LoadUndefined { dst } => op(h_load, 0, 0, dst, 0, Value::UNDEFINED.bits()),
        Instr::LoadConst { dst, idx } => match f.constants.get(idx as usize) {
            Some(c) if c.is_number() => op(h_load, 0, 0, dst, 0, c.bits()),
            Some(_) => op(h_load_const, 0, 0, dst, 0, idx as u64),
            None => leave,
        },
        Instr::FuseConstEqJump { ldst, idx, a, dst, when, target }
            if a != ldst && (idx as usize) < f.constants.len() && fits(target) =>
        {
            op(h_const_eq_jump, a, ldst, dst, at(target), idx as u64 | (when as u64) << 32)
        }
        Instr::Move { dst, src } => op(h_move, src, 0, dst, 0, 0),
        Instr::FuseMove2 { dst, src, dst2, src2 } => op(h_move2, src, src2, dst, dst2 as u32, 0),
        Instr::Add { dst, a, b } => op(h_add, a, b, dst, 0, 0),
        Instr::Sub { dst, a, b } => op(h_sub, a, b, dst, 0, 0),
        Instr::Mul { dst, a, b } => op(h_mul, a, b, dst, 0, 0),
        Instr::Div { dst, a, b } => op(h_div, a, b, dst, 0, 0),
        Instr::Mod { dst, a, b } => op(h_mod, a, b, dst, 0, 0),
        Instr::Neg { dst, a } => op(h_neg, a, 0, dst, 0, 0),
        Instr::Not { dst, a } => op(h_not, a, 0, dst, 0, 0),
        Instr::AddInt { dst, a, imm, .. } => op(h_add_int, a, 0, dst, 0, imm as u32 as u64),
        Instr::FuseAddIntMove { dst, a, imm, mdst, msrc, .. } => {
            op(h_add_int_move, a, msrc, dst, mdst as u32, imm as u32 as u64)
        }
        Instr::FuseAddLoadInt { ldst, val, dst, a } if a != ldst => op(h_add_load_int, a, ldst, dst, 0, lit(val)),
        Instr::Bitwise { dst, a, b, op: bop } => {
            op(bitwise_handler(bop, [h_and, h_or, h_xor, h_shl, h_shr, h_ushr]), a, b, dst, 0, 0)
        }
        Instr::FuseLoadIntBitwise { ldst, val, dst, a, b, op: bop } if b == ldst && a != ldst => {
            let h = bitwise_handler(bop, [h_li_and, h_li_or, h_li_xor, h_li_shl, h_li_shr, h_li_ushr]);
            op(h, a, ldst, dst, 0, lit(val))
        }
        Instr::Eq { dst, a, b } => op(h_eq, a, b, dst, 0, 0),
        Instr::Ne { dst, a, b } => op(h_eq, a, b, dst, 0, 1),
        Instr::Lt { dst, a, b } => op(h_lt, a, b, dst, 0, 0),
        Instr::Le { dst, a, b } => op(h_le, a, b, dst, 0, 0),
        Instr::Gt { dst, a, b } => op(h_gt, a, b, dst, 0, 0),
        Instr::Ge { dst, a, b } => op(h_ge, a, b, dst, 0, 0),
        Instr::JumpIfNotLt { a, b, target } if fits(target) && target as usize > ip => {
            op(h_jump_not_lt, a, b, 0, at(target), 0)
        }
        Instr::JumpIfNotLe { a, b, target } if fits(target) && target as usize > ip => {
            op(h_jump_not_le, a, b, 0, at(target), 0)
        }
        Instr::FuseIntCmpJump { ldst, val, a, le, target } if a != ldst && fits(target) && target as usize > ip => {
            op(if le { h_int_le_jump } else { h_int_lt_jump }, a, ldst, 0, at(target), lit(val))
        }
        Instr::FuseIntEqJump { ldst, val, a, dst, target } if a != ldst && fits(target) => {
            op(h_int_eq_jump, a, ldst, dst, at(target), lit(val))
        }
        Instr::FuseIntNeJump { ldst, val, a, dst, target } if a != ldst && fits(target) => {
            op(h_int_ne_jump, a, ldst, dst, at(target), lit(val))
        }
        Instr::FuseEqJump { dst, a, b, ne, when, target } if fits(target) => {
            op(h_eq_jump, a, b, dst, at(target), ne as u64 | (when as u64) << 1)
        }
        Instr::FuseNotJump { dst, a, when, target } if fits(target) && target as usize > ip => {
            op(h_not_jump, a, 0, dst, at(target), when as u64)
        }
        Instr::JumpIfFalse { cond, target } if fits(target) => {
            op(if target as usize > ip { h_jump_if } else { h_jump_if_back }, cond, 0, 0, at(target), 0)
        }
        Instr::JumpIfTrue { cond, target } if fits(target) => {
            op(if target as usize > ip { h_jump_if } else { h_jump_if_back }, cond, 0, 0, at(target), 1)
        }
        Instr::Jump { target } if fits(target) => {
            op(if target as usize > ip { h_jump } else { h_jump_back }, 0, 0, 0, at(target), 0)
        }
        Instr::FuseAddIntJump { dst, a, imm, target, .. } if fits(target) => {
            op(h_add_int_jump, a, 0, dst, at(target), imm as u32 as u64)
        }
        Instr::LoadGlobal { dst, idx } if global(idx) => op(h_load_global, 0, 0, dst, idx, 0),
        Instr::StoreGlobal { idx, src } | Instr::StoreGlobalStrict { idx, src } | Instr::StoreGlobalResolved { idx, src }
            if global(idx) =>
        {
            op(h_store_global, src, 0, 0, idx, 0)
        }
        Instr::FuseLoadGlobal2 { dst, idx, dst2, idx2 } if global(idx) && global(idx2) => {
            op(h_load_global2, dst2, 0, dst, idx, idx2 as u64)
        }
        Instr::FuseGlobalCmpJump { gdst, idx, a, le, target }
            if global(idx) && a != gdst && fits(target) && target as usize > ip =>
        {
            op(if le { h_global_le_jump } else { h_global_lt_jump }, a, 0, gdst, at(target), idx as u64)
        }
        Instr::FuseLoadGlobalAddInt { gdst, idx, dst, a, imm, .. } if global(idx) => {
            op(h_load_global_add_int, a, gdst, dst, idx, imm as i32 as u32 as u64)
        }
        Instr::FuseGlobalIntCmpJump { gdst, idx, ldst, val, target }
            if global(idx as u32) && gdst != ldst && fits(target) && target as usize > ip =>
        {
            op(h_global_int_lt_jump, gdst, ldst, idx, at(target), lit(val))
        }
        Instr::FuseGlobalIncJump { reg, idx, imm, target, .. } if global(idx) && idx <= u16::MAX as u32 && fits(target) => {
            op(h_global_inc_jump, idx as u16, 0, reg, at(target), imm as i32 as u32 as u64)
        }
        Instr::FuseGlobalInc { reg, idx, imm, .. } if global(idx) && idx <= u16::MAX as u32 => {
            op(h_global_inc, idx as u16, 0, reg, 0, imm as i32 as u32 as u64)
        }
        Instr::FuseIntBitwiseStoreGlobal { ldst, val, dst, a, op: bop, idx } if global(idx) && a != ldst => {
            let h = bitwise_handler(bop, [h_st_and, h_st_or, h_st_xor, h_st_shl, h_st_shr, h_st_ushr]);
            op(h, a, ldst, dst, idx, lit(val as i32))
        }
        Instr::FuseGlobalIntBitwise { gdst, idx, ldst, val, dst, op: bop } if global(idx) && gdst != ldst => {
            let h = bitwise_handler(bop, [h_gl_and, h_gl_or, h_gl_xor, h_gl_shl, h_gl_shr, h_gl_ushr]);
            op(h, gdst, ldst, dst, idx, lit(val as i32))
        }
        Instr::GetIndex { dst, obj, key } => op(h_get_index, obj, key, dst, 0, 0),
        Instr::SetIndex { obj, key, val } => op(h_set_index, obj, key, val, 0, 0),
        Instr::GetProp { dst, obj, name } if named(name, "length") => op(h_get_length, obj, 0, dst, func_id, 0),
        // A private name's read or write is the dispatch arm's (brand
        // checks): it fills no way of the cache these two ask.
        Instr::GetProp { dst, obj, name } if !private(name) => {
            // A name of at most 8 bytes: its key word for the (shape, key)
            // memo, which the light handler asks inline (`GETPROP_WORDS`).
            let key = f.string_constants[name as usize].as_bytes();
            #[cfg(feature = "jit")]
            let mega = super::ic::ic_mega_enabled();
            #[cfg(not(feature = "jit"))]
            let mega = false;
            // A site that has read through its prototype chain starts at
            // the handler for that (`h_get_prop_proto`).
            let h: Handler = if proto_site(ip) { h_get_prop_proto } else { h_get_prop };
            if key.len() <= 8 && mega {
                op(h, obj, GETPROP_WORDS | key.len() as u16, dst, func_id, super::ic::key_words(key).0)
            } else {
                op(h, obj, 0, dst, func_id, 0)
            }
        }
        Instr::SetProp { obj, val, name, strict } if !private(name) => {
            op(h_set_prop, obj, val, strict as u16, func_id, name as u64)
        }
        Instr::GetPropDv { dst, obj, slot, .. } => op(h_get_prop_dv, obj, 0, dst, func_id, slot as u64),
        Instr::CallMethodDv {
            dst,
            obj,
            arg_base,
            argc,
            slot,
            ..
        } => op(h_call_dv, obj, arg_base, dst, argc as u32, slot as u64),
        Instr::CallWithThisDv {
            dst,
            callee,
            this_v,
            arg_base,
            argc,
            ..
        } => op(h_call_this_dv, callee, this_v, dst, arg_base as u32, argc as u64),
        Instr::MathOp { .. } => op(h_math, 0, 0, 0, 0, 0),
        Instr::FuseIntSwitch { .. } | Instr::FuseStrSwitch { .. } => op(h_switch, 0, 0, 0, func_id, 0),
        // A private method's call is the dispatch arm's (its brand check).
        Instr::CallMethod {
            dst,
            obj,
            name,
            arg_base,
            argc,
        } if !private(name) => {
            let which = match argc {
                1 if named(name, "push") => METHOD_PUSH,
                1 if named(name, "charCodeAt") => METHOD_CHAR_CODE_AT,
                0 if named(name, "pop") => METHOD_POP,
                _ => 0,
            };
            op(h_call_method, obj, arg_base, dst, func_id, which)
        }
        Instr::Call {
            dst,
            callee,
            arg_base,
            argc,
        } => op(h_call, callee, arg_base, dst, func_id, argc as u64),
        Instr::CallWithThis {
            dst,
            callee,
            this_v,
            arg_base,
            argc,
            name,
        } => op(h_call_this, callee, this_v, dst, arg_base as u32, argc as u64 | (name as u64) << 32),
        Instr::FuseAddReturn { dst, a, b, src } => op(h_add_return, a, b, dst, src as u32, 0),
        Instr::Return { src } => op(h_return, src, 0, 0, 0, 0),
        Instr::ReturnUndefined => op(h_return_undefined, 0, 0, 0, 0, 0),
        Instr::UpvalGet { dst, idx } => op(h_upval_get, 0, 0, dst, idx as u32, 0),
        Instr::FuseUpvalGetMove { dst, idx, mdst, msrc } => op(h_upval_get_move, msrc, mdst, dst, idx as u32, 0),
        Instr::UpvalSet { idx, src } => op(h_upval_set, src, 0, 0, idx as u32, 0),
        Instr::CellGet { dst, cell } => op(h_cell_get, cell, 0, dst, 0, 0),
        Instr::CellSet { cell, src } => op(h_cell_set, cell, src, 0, 0, 0),
        Instr::IterNext {
            value_dst,
            done_dst,
            iter,
            idx,
            ..
        } => op(h_iter_next, iter, idx, value_dst, done_dst as u32, 0),
        Instr::NewArray { dst, arg_base, argc } => op(h_new_array, arg_base, 0, dst, argc as u32, 0),
        Instr::NewObject { dst, hint } => op(h_new_object, 0, 0, dst, 0, hint as u64),
        Instr::FinalizeObject {
            dst,
            plan,
            val_base,
            count,
        } => op(h_finalize_object, val_base, count, dst, func_id, plan as u64),
        Instr::MakeClosure { dst, func_id: f } => op(h_make_closure, 0, 0, dst, f, 0),
        Instr::MakeArrow {
            dst,
            func_id: f,
            this_reg,
        } => op(h_make_closure, this_reg, 0, dst, f, 1),
        Instr::MakeCell { reg } => op(h_make_cell, 0, 0, reg, 0, 0),
        Instr::MakeCellTdz { reg } => op(h_make_cell, 0, 0, reg, 0, 1),
        Instr::PushHandler { .. } | Instr::PushFinally { .. } => op(h_push_handler, 0, 0, 0, 0, 0),
        Instr::PopHandler | Instr::PopFinally => op(h_pop_handler, 0, 0, 0, at(ip as u32 + 1), 0),
        Instr::FusePopHandlerJump { target } if fits(target) && target as usize > ip => {
            op(h_pop_handler, 0, 0, 0, at(target), 0)
        }
        Instr::EndFinally { kind_reg, .. } => op(h_end_finally, kind_reg, 0, 0, 0, 0),
        Instr::FuseTypeOfJump { dst, a, code: c, neg, when, target } if fits(target) && target as usize > ip => {
            op(h_typeof_jump, a, 0, dst, at(target), c as u64 | (neg as u64) << 8 | (when as u64) << 9)
        }
        Instr::FuseNullJump { ldst, a, dst, ne, when, target } if fits(target) && target as usize > ip => {
            op(h_null_jump, a, ldst, dst, at(target), ne as u64 | (when as u64) << 1)
        }
        Instr::LooseEq { dst, a, b } => op(h_loose_eq, a, b, dst, 0, 0),
        Instr::LooseNe { dst, a, b } => op(h_loose_eq, a, b, dst, 0, 1),
        Instr::StrConcat { dst, a, b } => op(h_add_any, a, b, dst, 0, 0),
        Instr::StrConcatChain { dst, a, b } => op(h_str_chain, a, b, dst, 0, 0),
        Instr::StrAppendInPlace { dst, a, b } => op(h_str_append, a, b, dst, 0, 0),
        Instr::ToStr { dst, a } => op(h_to_str, a, 0, dst, 0, 0),
        Instr::StrAppendIndex { .. } => op(h_str_append_index, 0, 0, 0, 0, 0),
        Instr::GetIterator { dst, src } => op(h_get_iterator, src, 0, dst, 0, 0),
        _ => leave,
    }
}

/// A function's positions, built at the first back-edge that asks.
struct LoopFn {
    /// The dispatched code these were built from (a function's body can be
    /// replaced: `engine::lazy_install`).
    code: usize,
    ops: Box<[Op]>,
    /// Per position: the strikes of the entries at it (a loop's head),
    /// `MAX_STRIKES` of which leave that loop to the dispatch loop.
    strikes: Box<[u8]>,
    /// The function's loop heads (the distinct targets of its backward
    /// jumps, sorted), and how many of them the runner has given back:
    /// when that is all of them the function's back-edges stop asking
    /// (`ST_DEAD`). `open`: a loop was given back at a head not among
    /// them, so the count proves nothing.
    heads: Box<[u32]>,
    gone: u32,
    open: bool,
}

/// A callee `Vm::loop_resolve` resolved: its identity and heap version, and
/// its function, closure and frame description.
#[derive(Clone, Copy)]
struct RunCall {
    bits: u64,
    ver: u32,
    fid: u32,
    closure: u32,
    desc: u32,
}

impl RunCall {
    const EMPTY: RunCall = RunCall {
        bits: u64::MAX,
        ver: 0,
        fid: 0,
        closure: 0,
        desc: 0,
    };
}

/// `LoopTier::calls`' slots.
const RUN_CALL_SLOTS: usize = 256;

/// Back-edges a function's loops take in the dispatch loop before the
/// runner is asked. Building a function's positions costs about what a few
/// hundred handled instructions save, and most loops of a large program
/// never run that long: at 32 the two shorter bundles paid one to two
/// percent for positions they barely used.
const WARM_BACK_EDGES: u16 = 256;
/// The strikes at one loop's head after which that loop stays in the
/// dispatch loop. An entry is a round trip of a few hundred instructions,
/// which the handlers of an iteration or two do not save: an entry that
/// left before a back-edge (an instruction with no handler on every
/// iteration's path) is two strikes, one that took a single back-edge (a
/// loop that runs twice and returns) is one, and one that took eight or
/// more takes a strike off. An entry that left in a frame the handlers
/// pushed is dearer (the dispatch loop takes over a callee, and returns
/// through frames it did not enter) and its iterations, being calls, saved
/// less: under eight back-edges it is a strike, and only sixteen or more
/// take one off. A request loop whose every fourth request built a string
/// in a handler function, at an instruction the handlers did not run, ran
/// 17% slower entered than not and was never given back.
const MAX_STRIKES: u8 = 64;

/// A given-back loop's strikes, once it is counted in `LoopFn::gone`.
const GONE: u8 = u8::MAX;

const ST_COLD: u8 = 0;
const ST_READY: u8 = 1;
const ST_DEAD: u8 = 2;

/// Entries of the table of loops the runner gave back (`LoopTier::dead`). A
/// large program has hundreds of them, and one a collision pushes out pays
/// a call at its every back-edge until it is put back.
const DEAD_SLOTS: usize = 4096;

/// The runner's per-VM tables.
pub(crate) struct LoopTier {
    /// Per function id: `ST_*`.
    state: Vec<u8>,
    /// Per function id: back-edges counted while cold.
    warm: Vec<u16>,
    fns: Vec<Option<Box<LoopFn>>>,
    /// Loops the runner gave back, as `dead_key`s, each in the slot its key
    /// names: a back-edge of one costs the dispatch arm a load and a
    /// compare. A loop a collision pushed out is found again by its strikes.
    /// Empty until the first loop is given back (`DEAD_SLOTS` words then):
    /// a VM that never runs a loop that long carries no table.
    dead: Vec<u64>,
    /// The dispatched code of the function the runner is in (`h_math` and
    /// `h_switch` read their instruction's fields from it).
    code: usize,
    /// That function's first position (a position's index is its distance
    /// from it).
    ops: usize,
    /// The first register of the frame the runner is in, and the budget
    /// across a `heavy!` handler's body.
    regs: usize,
    fuel: u64,
    /// The closure of the frame the runner is in (its captured variables),
    /// and its function.
    closure: u32,
    func: u32,
    /// The upvalue list of `closure` as (address, length), taken at the
    /// frame's first captured-variable read and dropped at every frame
    /// change (`Vm::loop_frame_is`, and an entry's end), as the dispatch
    /// loops keep theirs: a closure's list is fixed when `MakeClosure` makes
    /// it, and the running frame roots the closure. 0: not taken.
    upv: usize,
    upv_len: usize,
    /// Callees the call handlers resolved (`Vm::loop_resolve`), each in the
    /// slot its heap index names. Empty until the first call.
    calls: Vec<RunCall>,
    /// The stack pointer below which a back-edge returns to the runner's
    /// loop (`back`).
    stack_floor: usize,
    /// Back-edges that found the stack below `stack_floor`.
    stack_trips: u64,
    /// Entries that ran a handler chain.
    entries: u64,
    /// The frames there were when the runner was entered: the ones past
    /// them are frames its calls pushed, which its returns pop.
    floor: usize,
    /// Frames the runner pushed, over all entries.
    pushes: u64,
    /// The error a handler's work raised (`THREW`), until the dispatch loop
    /// takes it (`Vm::loop_thrown`).
    thrown: Option<Thrown>,
}

impl Default for LoopTier {
    fn default() -> Self {
        LoopTier {
            state: Vec::new(),
            warm: Vec::new(),
            fns: Vec::new(),
            dead: Vec::new(),
            code: 0,
            ops: 0,
            regs: 0,
            fuel: 0,
            closure: 0,
            func: 0,
            upv: 0,
            upv_len: 0,
            calls: Vec::new(),
            stack_floor: 0,
            stack_trips: 0,
            entries: 0,
            floor: 0,
            pushes: 0,
            thrown: None,
        }
    }
}

/// The loop headed at `head` of `func_id`, as one word.
#[inline(always)]
fn dead_key(func_id: u32, head: usize) -> u64 {
    (func_id as u64) << 32 | head as u32 as u64
}

/// The slot of [`LoopTier::dead`] a key names.
#[inline(always)]
fn dead_slot(key: u64) -> usize {
    // The function's id spread over the table, the head beside it.
    let (func, head) = ((key >> 32) as u32, key as u32);
    ((func.wrapping_mul(0x9E37_79B1) >> 20) ^ head) as usize & (DEAD_SLOTS - 1)
}

env_off_switch! {
    /// `ZIPP_NO_LOOP_RUNNER=1`: every loop stays in the dispatch loop (A/B).
    fn loop_runner_enabled() = "ZIPP_NO_LOOP_RUNNER"
}

env_off_switch! {
    /// `ZIPP_NO_LOOP_FRAMES=1`: the runner pushes no frame; a call that
    /// needs one is the dispatch loop's (A/B).
    fn loop_frames_enabled() = "ZIPP_NO_LOOP_FRAMES"
}

/// What `Vm::loop_back_edge` answers when the runner left in another frame
/// than the one it was entered in (a frame a call of its pushed), whose
/// position it stored in that frame: the dispatch loop goes on from the top
/// frame, as after any call.
pub(crate) const LOOP_FRAMES: usize = usize::MAX;

/// What `Vm::loop_back_edge` answers when a handler's work raised an error:
/// the dispatch loop returns it (`Vm::loop_thrown`), as it returns one an
/// arm of its own raised. Every answer from here up is one of these two.
pub(crate) const LOOP_THROWN: usize = usize::MAX - 1;

/// Whether the environment variable `name` is set, asked once (`cell`).
fn env_latch(cell: &std::sync::atomic::AtomicU8, name: &str) -> bool {
    use std::sync::atomic::Ordering;
    match cell.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = crate::env_var_os(name).is_some();
            cell.store(v as u8, Ordering::Relaxed);
            v
        }
    }
}

/// `ZIPP_LOOP_RUNNER_EAGER=1`: a loop is the runner's from its first
/// back-edge (for tests: every loop of a short program goes through it).
fn loop_runner_eager() -> bool {
    static ON: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(2);
    env_latch(&ON, "ZIPP_LOOP_RUNNER_EAGER")
}

/// `ZIPP_LOOP_RUNNER_TRACE=1`: every entry's outcome on stderr (how many
/// back-edges it took, and the instruction it left at).
fn loop_runner_trace() -> bool {
    static ON: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(2);
    env_latch(&ON, "ZIPP_LOOP_RUNNER_TRACE")
}

impl<'p> Vm<'p> {
    /// The dispatch loop has taken a back-edge to `head` in `func_id`, whose
    /// dispatched code is `code` and whose window starts at `base`: where it
    /// continues. `head` itself when the runner did not take the loop, else
    /// the position the runner left at.
    #[inline(always)]
    pub(crate) fn loop_back_edge(&mut self, func_id: u32, base: usize, head: usize, code: &'p [Instr]) -> usize {
        match self.lite.loops.state.get(func_id as usize) {
            Some(&ST_READY) => {
                let key = dead_key(func_id, head);
                if self.lite.loops.dead.get(dead_slot(key)) == Some(&key) {
                    head
                } else {
                    self.loop_run(func_id, base, head, code)
                }
            }
            Some(&ST_DEAD) => head,
            _ => self.loop_warm(func_id, head),
        }
    }

    /// A back-edge of a function the runner has not been asked about yet.
    #[cold]
    #[inline(never)]
    fn loop_warm(&mut self, func_id: u32, head: usize) -> usize {
        let i = func_id as usize;
        let t = &mut self.lite.loops;
        if t.state.len() <= i {
            t.state.resize(i + 1, ST_COLD);
            t.warm.resize(i + 1, 0);
        }
        // On x86-64 every handler's hand-off is checked to be a jump (the
        // unit test below, and the handlers' machine code). Elsewhere a
        // hand-off that is a call would nest the native stack through a
        // recursion that takes no back-edge, so the runner stays off there
        // until the same is checked.
        if !loop_runner_enabled() || !cfg!(target_arch = "x86_64") {
            t.state[i] = ST_DEAD;
            return head;
        }
        t.warm[i] += 1;
        if t.warm[i] >= WARM_BACK_EDGES || loop_runner_eager() {
            t.state[i] = ST_READY;
        }
        head
    }

    /// Whether every initialized global slot reads and writes as the slot:
    /// no own property of the global object routes one, no evalScript
    /// `const`, no pending unresolvable reference (what `LoadGlobal` and
    /// `global_store_plain` test per slot). Only program code changes any
    /// of it, and the runner's handlers run none.
    fn loop_globals_plain(&self) -> bool {
        self.global_route_epoch == 0 && self.eval_const_globals.is_empty() && self.strict_unresolvable_globals.is_empty()
    }

    /// The runner's positions for `func_id`, whose dispatched code is
    /// `code`: built at the first ask, and again when that code is another
    /// (a deferred body replaced its stub).
    #[inline(always)]
    fn loop_fn(&mut self, func_id: u32, code: &'p [Instr]) -> *const Op {
        match self.lite.loops.fns.get(func_id as usize) {
            Some(Some(f)) if f.code == code.as_ptr() as usize => f.ops.as_ptr(),
            _ => self.loop_fn_build(func_id, code),
        }
    }

    #[cold]
    #[inline(never)]
    fn loop_fn_build(&mut self, func_id: u32, code: &'p [Instr]) -> *const Op {
        let i = func_id as usize;
        if self.lite.loops.fns.len() <= i {
            self.lite.loops.fns.resize_with(i + 1, || None);
        }
        // The function's code changed: none of its loops is given back.
        if self.lite.loops.fns[i].is_some() {
            for k in self.lite.loops.dead.iter_mut() {
                if (*k >> 32) as u32 == func_id {
                    *k = u64::MAX;
                }
            }
        }
        let f = self.func(i);
        let nglobals = self.globals.len();
        let proto_site = |ip: usize| self.ic_way0_is_proto(func_id, ip);
        let ops: Box<[Op]> = (0..code.len()).map(|ip| decode(f, code, ip, func_id, nglobals, &proto_site)).collect();
        let at = ops.as_ptr();
        // Its loop heads: the distinct targets of its backward jumps.
        let mut heads: Vec<u32> = code
            .iter()
            .enumerate()
            .filter_map(|(ip, ins)| super::lite::jump_target(ins).filter(|&t| t as usize <= ip))
            .collect();
        heads.sort_unstable();
        heads.dedup();
        self.lite.loops.fns[i] = Some(Box::new(LoopFn {
            code: code.as_ptr() as usize,
            ops,
            strikes: vec![0; code.len()].into_boxed_slice(),
            heads: heads.into_boxed_slice(),
            gone: 0,
            open: false,
        }));
        at
    }

    /// `CallMethod` on an ordinary object or class instance past the lite
    /// loop's entry: the method cache's ways by the receiver's mirror, as
    /// the dispatch arm asks them (every layout the site has seen), then
    /// the cache, which fills both. Out of line: a site one layout reaches
    /// never gets here, and inline these steps grew the handler's body
    /// under it.
    #[inline(never)]
    fn loop_method_other(&mut self, at: &Instr, func_id: u32, ip: usize, recv: Value, name: u32) -> Option<(u32, u32, Value, u32)> {
        #[cfg(all(feature = "jit", not(feature = "safe-sandbox")))]
        if let Some((fid, closure, callee)) = self.ic_call_method_mirror(func_id, ip, recv) {
            return Some((fid, closure, callee, self.lean_call_desc(fid)));
        }
        self.lite_method(at, func_id, ip, recv, name)
    }

    /// A `Call`'s plain-function callee: its function, closure and frame
    /// description (`lean_call_desc`), by the callee's identity and heap
    /// version, else the site's call cache (`ic_call`). The lite loop's
    /// cache (`lite_resolve`) keeps only the functions the lite loop runs, so
    /// a call of any other one (a closure of a function with a `try`, a
    /// generator's maker) recomputed its description from the function's
    /// record on every call.
    #[inline(always)]
    fn loop_resolve(&mut self, func_id: u32, ip: usize, callee: Value) -> Option<(u32, u32, u32)> {
        if callee.is_heap() {
            let idx = callee.heap_index();
            if let Some(e) = self.lite.loops.calls.get(idx as usize & (RUN_CALL_SLOTS - 1)) {
                if e.bits == callee.bits() && e.ver == self.heap.version_of(idx) {
                    return Some((e.fid, e.closure, e.desc));
                }
            }
        }
        self.loop_resolve_miss(func_id, ip, callee)
    }

    /// [`Self::loop_resolve`] past its cache, which it fills: the site's call
    /// cache, else the plain function itself (`ic_plain_fn`, what that cache
    /// fills from). A site that many callees reach in turn (a table of
    /// sixteen closures) disables its cache, which then answers nothing.
    #[inline(never)]
    fn loop_resolve_miss(&mut self, func_id: u32, ip: usize, callee: Value) -> Option<(u32, u32, u32)> {
        let (fid, closure) = match self.ic_call(func_id, ip, callee) {
            Some(call) => (call.fid, call.closure),
            None => self.ic_plain_fn(callee)?,
        };
        let desc = self.lean_call_desc(fid);
        if callee.is_heap() {
            let idx = callee.heap_index();
            if self.lite.loops.calls.is_empty() {
                self.lite.loops.calls = vec![RunCall::EMPTY; RUN_CALL_SLOTS];
            }
            self.lite.loops.calls[idx as usize & (RUN_CALL_SLOTS - 1)] = RunCall {
                bits: callee.bits(),
                ver: self.heap.version_of(idx),
                fid,
                closure,
                desc,
            };
        }
        Some((fid, closure, desc))
    }

    /// `GetProp` of a heap receiver past the site's ways: the dispatch
    /// arm's steps that run no code and raise nothing, in its
    /// order (a private name is never asked here: `decode`). The cache's
    /// inherited-data hit; a receiver with no shape by its kind (a class's
    /// static, a function's own property, a prototype intrinsic), a
    /// constructor's own data property, a method of `Array.prototype` or
    /// `String.prototype` itself; then the cache. `GetAct::None`: the arm's
    /// (a Proxy's trap, the member walk).
    #[inline(never)]
    fn loop_get_prop(&mut self, func_id: u32, ip: usize, o: Value, name: u32) -> GetAct {
        let key: &'p str = self.func(func_id as usize).string_constants[name as usize].as_str();
        if let Some(v) = self.ic_get_prop_proto_hit(func_id, ip, o, key) {
            self.ic_proto_hit_settle(o);
            return GetAct::Value(v);
        }
        let idx = o.heap_index();
        let (object, ctor, proxy) = match self.heap.get(idx) {
            HeapObj::Object(m) => (true, m.is_ctor, false),
            HeapObj::Proxy { .. } => (false, false, true),
            _ => (false, false, false),
        };
        if proxy {
            return GetAct::None;
        }
        if !object {
            if let Some(v) = self.shapeless_read(o, key) {
                return GetAct::Value(v);
            }
        } else if ctor {
            if let Some(v) = self.ctor_own_data_read(o, key) {
                return GetAct::Value(v);
            }
        } else if self.arr_proto != 0
            && (idx == self.arr_proto || idx == self.str_proto)
            && self.realm_global_objs.is_empty()
            && self.active_realm.is_none()
        {
            if let Some(bits) = self.proto_intrinsic_bits(key, idx == self.arr_proto) {
                return GetAct::Value(Value::from_bits(bits));
            }
        }
        self.ic_get_prop(func_id, ip, o, key)
    }

    /// `CallWithThis` of a native callee, as the dispatch arm runs one with
    /// no other realm in existence, in its order: a Map's or Set's method on
    /// a receiver of its kind; the captured-intrinsic lanes; the native
    /// itself. `None`, with nothing done, for anything else (`f.call`,
    /// `f.apply`, a callee that is not a native, another realm): the arm's.
    #[allow(clippy::too_many_arguments)]
    #[inline(never)]
    fn loop_call_this_native(
        &mut self,
        func_id: u32,
        name: u32,
        callee: Value,
        this_val: Value,
        base: usize,
        arg_base: u16,
        argc: u16,
    ) -> Result<Option<Value>, Thrown> {
        if !callee.is_heap() || !self.realm_global_objs.is_empty() || !self.realm_fns.is_empty() {
            return Ok(None);
        }
        let HeapObj::Native(id) = *self.heap.get(callee.heap_index()) else {
            return Ok(None);
        };
        if id == native::FN_CALL || id == native::FN_APPLY {
            return Ok(None);
        }
        if this_val.is_heap() {
            let kind_ok = match native::proto_method(id) {
                Some((_, 3, _)) => matches!(self.heap.get(this_val.heap_index()), HeapObj::Set(_)),
                Some((_, 4, _)) => matches!(self.heap.get(this_val.heap_index()), HeapObj::Map { .. }),
                _ => false,
            };
            if kind_ok {
                return self
                    .with_argv(base, arg_base, argc, |vm, argv| vm.call_native(id, this_val, argv))
                    .map(Some);
            }
        }
        if name != crate::bytecode::NO_NAME {
            if let Some(v) = self.captured_intrinsic_lane(func_id, base, this_val, callee, name, arg_base, argc)? {
                return Ok(Some(v));
            }
        }
        self.with_argv(base, arg_base, argc, |vm, argv| vm.call_native(id, this_val, argv))
            .map(Some)
    }

    /// `CallMethod` on a receiver that is not an ordinary object, past the
    /// handler's own leaves: the dispatch arm's steps for a primitive
    /// string, an array, a Map, a Set and a number, in its order. The
    /// string, array and Map lanes; then, as for every receiver, a user's
    /// method the method cache finds (the arm's frame: `None`) before the
    /// builtin dispatcher. `None`, with nothing done, for every other
    /// receiver and for a name none of these serves: the arm's.
    #[allow(clippy::too_many_arguments)]
    #[inline(never)]
    fn loop_method_builtin(
        &mut self,
        func_id: u32,
        ip: usize,
        recv: Value,
        name: u32,
        base: usize,
        arg_base: u16,
        argc: u16,
    ) -> Result<Option<Value>, Thrown> {
        let key: &'p str = self.func(func_id as usize).string_constants[name as usize].as_str();
        if recv.is_heap() {
            let idx = recv.heap_index();
            // 0 a string, 1 an array, 2 a Map or WeakMap, 3 a Set.
            let kind = match self.heap.get(idx) {
                HeapObj::Str(_) | HeapObj::Cons { .. } => 0,
                HeapObj::Array(_) => 1,
                HeapObj::Map { .. } | HeapObj::WeakMap { .. } => 2,
                HeapObj::Set(_) => 3,
                _ => return Ok(None),
            };
            match kind {
                0 => {
                    if argc == 1 && key == "charCodeAt" {
                        let arg = self.get(base, arg_base);
                        if let Some(v) = self.interp_char_code_at_fast(recv, arg) {
                            return Ok(Some(v));
                        }
                    }
                    if let Some(v) = self.interp_string_method_fast(idx, key, base, arg_base, argc)? {
                        return Ok(Some(v));
                    }
                }
                1 => {
                    if let Some(v) = self.interp_array_lane(recv, key, base, arg_base, argc) {
                        return Ok(Some(v));
                    }
                }
                2 if argc == 1 || argc == 2 => {
                    if let Some(v) = self.interp_map_method_fast(recv, key, base, arg_base, argc)? {
                        return Ok(Some(v));
                    }
                }
                _ => {}
            }
        } else if !recv.is_number() {
            return Ok(None);
        }
        // `f.call`, `f.bind` and `f.apply` are a function receiver's, which
        // is none of these; a user's method takes the arm's frame.
        if self.ic_call_method(func_id, ip, recv, key).is_some() {
            return Ok(None);
        }
        self.try_builtin_method(recv, key, base, arg_base, argc)
    }

    /// The runner is in the top frame from here on: `func_id`'s, whose
    /// dispatched code starts at `code` and positions at `ops`, its window
    /// at `base`.
    #[inline(always)]
    fn loop_frame_is(&mut self, func_id: u32, closure: u32, code: usize, ops: *const Op, base: usize) {
        // SAFETY: `base` is a frame's first register, inside the file.
        let regs = unsafe { self.regs.as_mut_ptr().add(base) } as usize;
        let t = &mut self.lite.loops;
        t.code = code;
        t.ops = ops as usize;
        t.regs = regs;
        t.closure = closure;
        t.func = func_id;
        t.upv = 0;
    }

    /// A call the runner makes of `fid`, which is not a leaf: push its
    /// frame, as the lite loop's call does, and answer its first position;
    /// the handlers go on there, in that frame. `None`, with nothing
    /// changed, when the push is not the lean one (`lean_push_or_decline`):
    /// the dispatch loop's call. `recv`: a method call's receiver.
    #[allow(clippy::too_many_arguments)]
    #[inline(always)]
    fn loop_enter(
        &mut self,
        fid: u32,
        closure: u32,
        desc: u32,
        recv: Option<Value>,
        base: usize,
        arg_base: u16,
        argc: u16,
        dst: u16,
        ip_next: usize,
        callee: Value,
    ) -> Option<*const Op> {
        if !loop_frames_enabled() {
            return None;
        }
        // The native stack, as at a back-edge (`back`): a recursion takes
        // none, and its calls are where a chain whose hand-offs were not
        // jumps is cut.
        if stack_ptr() < self.lite.loops.stack_floor {
            self.lite.loops.stack_trips += 1;
            return None;
        }
        let p: &'p FuncProto = self.func(fid as usize);
        let code: &'p [Instr] = if self.interp_fuse_ok() {
            self.entered_code(fid, p)
        } else {
            p.code.as_slice()
        };
        let new_base = self.regs.len();
        if !self.lean_push_or_decline(fid, closure, desc, recv, base, arg_base, argc, dst, ip_next, callee) {
            return None;
        }
        let ops = self.loop_fn(fid, code);
        // The dispatch loops' frame-transition safe point.
        self.maybe_gc();
        self.loop_frame_is(fid, closure, code.as_ptr() as usize, ops, new_base);
        self.lite.loops.pushes += 1;
        Some(ops)
    }

    /// Whether a `Return` in the top frame is the runner's: a frame this
    /// entry pushed (the frame the runner was entered in returns in the
    /// dispatch loop: its return may end the run), with no `finally` to pass
    /// through, whose caller's positions are built (it ran here before its
    /// call).
    #[inline(always)]
    fn loop_returns(&self) -> bool {
        let n = self.frames.len();
        n > self.lite.loops.floor
            && self.frames[n - 1].handlers.is_empty()
            && matches!(self.lite.loops.fns.get(self.frames[n - 2].func as usize), Some(Some(_)))
    }

    /// A `Return` of `v` the runner makes (`loop_returns`): pop the top
    /// frame, hand `v` to its caller, and answer the caller's next
    /// position. Counts as a back-edge does: an entry that returned from a
    /// call got somewhere.
    #[inline(always)]
    fn loop_return_now(&mut self, v: Value) -> *const Op {
        let n = self.frames.len();
        let (func, base, ip, closure) = {
            let c = &self.frames[n - 2];
            (c.func, c.base, c.ip, c.closure)
        };
        let (ops, code) = match self.lite.loops.fns.get(func as usize) {
            Some(Some(f)) => (f.ops.as_ptr(), f.code),
            _ => unreachable!("the caller's positions"),
        };
        self.pop_frame_fast(v, usize::MAX);
        self.maybe_gc();
        self.loop_frame_is(func, closure, code, ops, base);
        let t = &mut self.lite.loops;
        if t.fuel > 1 {
            t.fuel -= 1;
        }
        // SAFETY: `ip` is the position after the caller's call.
        unsafe { ops.add(ip) }
    }

    /// [`Self::loop_return_now`] when the return is the runner's, else
    /// `None` with nothing changed: the dispatch loop's return.
    #[inline(always)]
    fn loop_return(&mut self, v: Value) -> Option<*const Op> {
        if !self.loop_returns() {
            return None;
        }
        Some(self.loop_return_now(v))
    }

    /// For the trace: the kinds of the operands of the instruction `at` of
    /// `fid`, in the top frame, that the runner left at (a property's
    /// holder and name, an element's holder and key, a callee).
    #[cold]
    #[inline(never)]
    fn loop_trace_operands(&self, fid: u32, at: &Instr) -> String {
        let base = self.frames[self.frames.len() - 1].base;
        let kind = |r: u16| -> String {
            let v = self.regs[base + r as usize];
            if !v.is_heap() {
                return self.type_of(v).to_string();
            }
            // The variant's name: the head of its debug form.
            struct Head(String);
            impl std::fmt::Write for Head {
                fn write_str(&mut self, s: &str) -> std::fmt::Result {
                    for ch in s.chars() {
                        if !ch.is_alphanumeric() {
                            return Err(std::fmt::Error);
                        }
                        self.0.push(ch);
                    }
                    Ok(())
                }
            }
            let mut h = Head(String::new());
            let _ = std::fmt::write(&mut h, format_args!("{:?}", self.heap.get(v.heap_index())));
            h.0
        };
        let name = |n: u32| self.func(fid as usize).string_constants.get(n as usize).map_or("?", |s| s.as_str());
        match *at {
            Instr::GetProp { obj, name: n, .. } | Instr::SetProp { obj, name: n, .. } | Instr::CallMethod { obj, name: n, .. } => {
                format!(" [{}.{}]", kind(obj), name(n))
            }
            Instr::GetIndex { obj, key, .. } | Instr::SetIndex { obj, key, .. } => format!(" [{}[{}]]", kind(obj), kind(key)),
            Instr::Add { a, b, .. } => format!(" [{} + {}]", kind(a), kind(b)),
            Instr::Call { callee, .. } => format!(" [{}()]", kind(callee)),
            Instr::CallWithThis { callee, this_v, .. } => format!(" [{}.call({})]", kind(callee), kind(this_v)),
            Instr::IterNext { iter, .. } => format!(" [{}]", kind(iter)),
            _ => String::new(),
        }
    }

    #[inline(never)]
    fn loop_run(&mut self, func_id: u32, base: usize, head: usize, code: &'p [Instr]) -> usize {
        let i = func_id as usize;
        // A loop the runner gave back stays the dispatch loop's: its every
        // back-edge comes here until the table holds it, so this is first.
        if let Some(Some(f)) = self.lite.loops.fns.get_mut(i) {
            if f.code == code.as_ptr() as usize && f.strikes[head] >= MAX_STRIKES {
                // Counted once (the strikes then read `GONE`); with every
                // loop of the function gone, its back-edges stop asking.
                let all = if f.strikes[head] != GONE {
                    f.strikes[head] = GONE;
                    match f.heads.binary_search(&(head as u32)) {
                        Ok(_) => f.gone += 1,
                        Err(_) => f.open = true,
                    }
                    !f.open && f.gone as usize >= f.heads.len()
                } else {
                    false
                };
                let key = dead_key(func_id, head);
                if self.lite.loops.dead.is_empty() {
                    self.lite.loops.dead = vec![u64::MAX; DEAD_SLOTS];
                }
                self.lite.loops.dead[dead_slot(key)] = key;
                if all {
                    self.lite.loops.state[i] = ST_DEAD;
                }
                return head;
            }
        }
        // A collection due now is the dispatch loop's to run, and under the
        // collector's stress mode every back-edge is.
        if self.heap.gc_requested() || self.gc_stress || !self.loop_globals_plain() {
            return head;
        }
        let ops = self.loop_fn(func_id, code);
        let floor = self.frames.len();
        let closure = self.frames[floor - 1].closure;
        // An entry under a handler's call (program code a native ran) puts
        // back what the handlers of the entry above it go on with.
        let outer = {
            let t = &self.lite.loops;
            (t.code, t.ops, t.regs, t.closure, t.func, t.floor, t.fuel, t.stack_floor)
        };
        self.loop_frame_is(func_id, closure, code.as_ptr() as usize, ops, base);
        let trips = self.lite.loops.stack_trips;
        let vm = self as *mut Self as *mut c_void;
        // SAFETY: the frame's window is in the register file, which no
        // handler grows (a call's push is one that fits it); a function's
        // positions are one per position of its code, and every jump target
        // `decode` kept is one of them.
        let (left, iters) = unsafe {
            let t = &mut self.lite.loops;
            t.floor = floor;
            t.stack_floor = stack_ptr().saturating_sub(STACK_SLACK);
            t.entries += 1;
            let mut op = ops.add(head);
            let mut fuel = FUEL;
            let mut iters = 0u64;
            loop {
                // The frame the chain is in now: a call or a return moved it.
                let regs = self.lite.loops.regs as *mut u64;
                let s = ((*op).h)(vm, regs, op, fuel);
                iters += fuel - s.fuel;
                if s.next & LEAVE != 0 {
                    break (s.next, iters);
                }
                op = s.next as *const Op;
                fuel = s.fuel;
                if fuel == 0 {
                    // The budget's end: the collector's poll, as the
                    // dispatch loop makes one at every back-edge.
                    if self.heap.gc_requested() {
                        break (op as usize, iters);
                    }
                    fuel = FUEL;
                }
            }
        };
        // Where the chain left: a position of the function it is in now.
        let (threw, paused) = (left & THREW != 0, left & PAUSE != 0);
        let left = left & !(LEAVE | THREW | PAUSE);
        let ip = (left - self.lite.loops.ops) / std::mem::size_of::<Op>();
        let top = self.frames.len() - 1;
        let moved = top + 1 != floor;
        if moved || threw {
            self.frames[top].ip = ip;
        }
        if !threw && !paused {
            if let Some(f) = &mut self.lite.loops.fns[i] {
                let n = &mut f.strikes[head];
                // `moved`: the entry left in a frame the handlers pushed.
                *n = match (iters, moved) {
                    (0, _) => n.saturating_add(2),
                    (1, _) | (2..=7, true) => n.saturating_add(1),
                    (2..=7, false) | (8..=15, true) => *n,
                    _ => n.saturating_sub(1),
                };
            }
        }
        if loop_runner_trace() && self.lite.loops.stack_trips != trips {
            eprintln!(
                "[loop] fn{i}: the handlers grew the native stack {} times: a hand-off was a call, not a jump",
                self.lite.loops.stack_trips - trips
            );
        }
        if loop_runner_trace() {
            let (fid, strikes) = (self.lite.loops.func, self.lite.loops.fns[i].as_ref().map_or(0, |f| f.strikes[head]));
            // SAFETY: `ip` is a position of the code the chain was in.
            let at = unsafe { &*(self.lite.loops.code as *const Instr).add(ip) };
            let what = self.loop_trace_operands(fid, at);
            if iters == 0 {
                eprintln!("[loop] fn{i} head {head}: left at {ip} before a back-edge (Some({at:?})), strike {strikes}, in fn{fid}{what}");
            } else {
                eprintln!("[loop] fn{i} head {head}: {iters} back-edges, left at {ip} (Some({at:?})), in fn{fid}{what}");
            }
        }
        {
            let t = &mut self.lite.loops;
            (t.code, t.ops, t.regs, t.closure, t.func, t.floor, t.fuel, t.stack_floor) = outer;
            t.upv = 0;
        }
        if threw {
            LOOP_THROWN
        } else if moved {
            LOOP_FRAMES
        } else {
            ip
        }
    }

    /// The error behind [`LOOP_THROWN`].
    #[cold]
    #[inline(never)]
    pub(crate) fn loop_thrown(&mut self) -> Thrown {
        self.lite.loops.thrown.take().unwrap_or_else(|| Thrown(String::new()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Loops through every kind of handler (arithmetic on Int32s and
    /// doubles, a script's globals, typed arrays, arrays, own and inherited
    /// properties, `length`, `charCodeAt` and `push`, `Math`, integer and
    /// string switches, a captured variable, DataView accessors, calls of a
    /// function and a method that take frames), each long enough to spend
    /// the runner's budget twice: the runner takes them and pushes the
    /// calls' frames itself, the program prints what Node prints for it,
    /// and in an optimized build no back-edge found the native stack grown,
    /// which is what a handler's call of the next not being a jump would
    /// show.
    #[test]
    fn handlers_chain_without_growing_the_stack() {
        let src = r#"
function arith(n) { var s = 0, d = 0.5; for (var i = 0; i < n; i++) { s = (s + i * 3 - (i >> 1)) | 0; d = d * 1.0001 + i / 7; if (i % 5 === 0) s ^= i; } return s + (d | 0); }
var g = 0; for (var gi = 0; gi < 10000; gi++) { g = (g + (gi & 7)) | 0; }
function typed(n) { var f = new Float64Array(64), k = new Int32Array(64), s = 0; for (var i = 0; i < n; i++) { f[i & 63] = f[(i + 1) & 63] + i; k[i & 63] = (k[(i + 3) & 63] + i) | 0; s = (s + k[i & 63]) | 0; } return s + (f[5] | 0); }
function arrays(n) { var a = [1, 2, 3, 4], s = 0; for (var i = 0; i < n; i++) { a[i & 3] = a[(i + 1) & 3] + 1; s = (s + a[i & 3] + a.length) | 0; } return s; }
function props(n) { var o = { x: 1, y: 2 }, p = Object.create({ z: 3 }), s = 0; for (var i = 0; i < n; i++) { o.x = o.y + (i & 1); s = (s + o.x + p.z) | 0; } return s; }
function strs(n) { var t = "hello world", a = [], s = 0; for (var i = 0; i < n; i++) { s = (s + t.charCodeAt(i % t.length)) | 0; if ((i & 255) === 0) a.push(i); } return s + a.length; }
function maths(n) { var s = 0; for (var i = 0; i < n; i++) { s = (Math.imul(s ^ i, 31) + Math.floor(i / 3)) | 0; } return s; }
function sw(n) { var s = 0, names = ["a", "b", "c", "d", "e", "f"]; for (var i = 0; i < n; i++) { switch (i & 7) { case 0: s += 1; break; case 1: s += 2; break; case 2: s += 3; break; case 3: s += 4; break; case 4: s += 5; break; default: s += 6; } switch (names[i % 6]) { case "a": s += 1; break; case "b": s += 2; break; case "c": s += 3; break; case "d": s += 4; break; case "e": s += 5; break; default: s += 7; } } return s; }
function clos(n) { var c = 0; return (function () { for (var i = 0; i < n; i++) { c = (c + i) | 0; } return c; })(); }
function dv(n) { var v = new DataView(new ArrayBuffer(16)), s = 0; for (var i = 0; i < n; i++) { v.setUint16(i & 7, i, (i & 1) === 0); s = (s + v.getUint8(i & 15) + v.getInt16(i & 7)) | 0; } return s; }
function inner(n) { var s = 0; for (var i = 0; i < n; i++) { s = (s + i * 3) | 0; } return s; }
class Box { constructor() { this.v = 1; } step(x) { var t = this.v; for (var i = 0; i < x; i++) { t = (t + i) | 0; } this.v = t & 1023; return t; } }
function calls(n) { var s = 0, b = new Box(); for (var i = 0; i < n; i++) { s = (s + inner(i & 7) + b.step(i & 3)) | 0; } return s; }
console.log([arith(10000), g, typed(10000), arrays(10000), props(10000), strs(10000), maths(10000), sw(10000), clos(10000), dv(10000), calls(10000)].join(","));
"#;
        let ast = crate::front::parse_auto(src).expect("parse");
        let program = crate::compile::compile_program(&ast, src).expect("compile");
        let mut vm = Vm::new(&program);
        #[cfg(all(feature = "jit", any(target_arch = "x86_64", target_arch = "aarch64")))]
        vm.set_jit_enabled(false);
        vm.run().expect("run");
        assert_eq!(
            vm.output,
            ["134775612,35000,-1537291438,16741666,55000,1014588,271319705,77912,49995000,24265399,5238420"]
        );
        if loop_runner_enabled() && vm.lite_entry_ok() && cfg!(target_arch = "x86_64") {
            assert!(vm.lite.loops.entries >= 10, "the runner took the loops: {}", vm.lite.loops.entries);
            if loop_frames_enabled() {
                assert!(vm.lite.loops.pushes >= 10000, "the runner pushed the calls' frames: {}", vm.lite.loops.pushes);
            }
            #[cfg(not(debug_assertions))]
            assert_eq!(vm.lite.loops.stack_trips, 0, "a handler's call of the next handler was not a jump");
        }
    }
}
