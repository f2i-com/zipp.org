//! Tier O's x86-64 emission.
//!
//! Registers: rbx is the register window, rsi the bail word, rdi the VM.
//! The allocator's values live in `alloc::GPRS` / `alloc::XMMS` or in
//! stack slots. A spilled operand is loaded into rdx (first) or r11
//! (second), xmm1 or xmm2; a spilled result is made in r10 or xmm3 and
//! stored. rax, rcx and xmm0 are each sequence's temporaries.
//!
//! A deopt stub per frame state boxes every value the state names into
//! the window at its register, stores the state's position to the bail
//! word, counts the deopt (`vm::jit_opt_deopt`, which may evict the body)
//! and leaves through the epilogue: the interpreter goes on there.

use dynasmrt::{dynasm, DynamicLabel, DynasmApi, DynasmLabelApi};

use super::alloc::{Alloc, Loc};
use super::build::{CallKind, Cmp, Func, Op, Rep, Term, V};
use crate::bytecode::BitwiseOp;
use crate::codegen::JitFn;
use crate::value::Value;

/// The outgoing area at the bottom of the frame: a helper's 32-byte shadow
/// space and its fifth argument, kept 16-aligned.
const OUT_AREA: i32 = 48;

const TAG_INT: u64 = 0x7FF9 << 48;
const TAG_BOOL: u64 = 0x7FFA << 48;
const TAG_NULL: u64 = 0x7FFB << 48;
const QNAN: u64 = 0x7FF8 << 48;
const SIGN: u64 = 1 << 63;
/// `|x| << 1` above this is a NaN (the infinities are exactly it).
const ABS2_INF: u64 = 0xFFE0 << 48;

const RAX: u8 = 0;
const RDX: u8 = 2;
const R10: u8 = 10;
const R11: u8 = 11;
const X1: u8 = 1;
const X2: u8 = 2;
const X3: u8 = 3;

type Asm = dynasmrt::x64::Assembler;

struct Em<'a> {
    ops: Asm,
    f: &'a Func,
    a: &'a Alloc,
    slot_base: i32,
    stubs: Vec<DynamicLabel>,
    epilogue: DynamicLabel,
    stress: bool,
}

impl Em<'_> {
    fn slot(&self, s: u32) -> i32 {
        self.slot_base + 8 * s as i32
    }

    fn rep(&self, v: V) -> Rep {
        self.f.insts[v as usize].rep
    }

    /// `v`'s GPR, loading a spilled one into `scratch`.
    fn gpr(&mut self, v: V, scratch: u8) -> u8 {
        match self.a.loc[v as usize] {
            Loc::Gpr(r) => r,
            Loc::Slot(s) => {
                let off = self.slot(s);
                dynasm!(self.ops ; mov Rq(scratch), [rsp + off]);
                scratch
            }
            Loc::Xmm(_) => unreachable!("value {v} is not in the GPR class"),
        }
    }

    /// `v`'s XMM register, loading a spilled one into `scratch`.
    fn xmm(&mut self, v: V, scratch: u8) -> u8 {
        match self.a.loc[v as usize] {
            Loc::Xmm(r) => r,
            Loc::Slot(s) => {
                let off = self.slot(s);
                dynasm!(self.ops ; movsd Rx(scratch), QWORD [rsp + off]);
                scratch
            }
            Loc::Gpr(_) => unreachable!("value {v} is not in the XMM class"),
        }
    }

    /// Where to make `v` (a GPR value), and the slot to store it to after.
    fn dst_gpr(&self, v: V) -> (u8, Option<i32>) {
        match self.a.loc[v as usize] {
            Loc::Gpr(r) => (r, None),
            Loc::Slot(s) => (R10, Some(self.slot(s))),
            Loc::Xmm(_) => unreachable!(),
        }
    }

    fn dst_xmm(&self, v: V) -> (u8, Option<i32>) {
        match self.a.loc[v as usize] {
            Loc::Xmm(r) => (r, None),
            Loc::Slot(s) => (X3, Some(self.slot(s))),
            Loc::Gpr(_) => unreachable!(),
        }
    }

    fn done_gpr(&mut self, d: u8, st: Option<i32>) {
        if let Some(off) = st {
            dynasm!(self.ops ; mov [rsp + off], Rq(d));
        }
    }

    fn done_xmm(&mut self, d: u8, st: Option<i32>) {
        if let Some(off) = st {
            dynasm!(self.ops ; movsd QWORD [rsp + off], Rx(d));
        }
    }

    /// The stress countdown before a guarding op (`opt::stress_every`).
    fn stress_check(&mut self, stub: DynamicLabel) {
        if self.stress {
            let at = &super::STRESS_COUNTDOWN as *const _ as i64;
            dynasm!(self.ops
                ; mov rax, QWORD at
                ; sub DWORD [rax], 1
                ; jz => stub
            );
        }
    }

    /// Frame state `s` into the window: each value it names boxed at its
    /// register. Uses rax, rcx, rdx and xmm0; every value stays where it is.
    fn write_state(&mut self, s: u32) {
        let f = self.f;
        for &(r, v) in &f.states[s as usize].regs {
            let off = 8 * r as i32;
            match self.rep(v) {
                Rep::Tagged => {
                    let rv = self.gpr(v, RAX);
                    dynasm!(self.ops ; mov [rbx + off], Rq(rv));
                }
                rep @ (Rep::I32 | Rep::Bool) => {
                    let rv = self.gpr(v, RDX);
                    let tag = if rep == Rep::I32 { TAG_INT } else { TAG_BOOL };
                    dynasm!(self.ops
                        ; mov eax, Rd(rv)
                        ; mov rcx, QWORD tag as i64
                        ; or rax, rcx
                        ; mov [rbx + off], rax
                    );
                }
                Rep::F64 => {
                    let xv = self.xmm(v, X1);
                    self.box_f64(xv, RDX);
                    dynasm!(self.ops ; mov [rbx + off], rdx);
                }
                Rep::Void => unreachable!("a frame state names values"),
            }
        }
    }

    /// `d = box(x)` by `Value::num`'s rule: an i32 when exact (not -0),
    /// the canonical NaN, else the double's bits. Uses rax, rcx, xmm0.
    fn box_f64(&mut self, x: u8, d: u8) {
        let not_int = self.ops.new_dynamic_label();
        let is_int = self.ops.new_dynamic_label();
        let nan = self.ops.new_dynamic_label();
        let done = self.ops.new_dynamic_label();
        dynasm!(self.ops
            ; cvttsd2si eax, Rx(x)
            ; cvtsi2sd xmm0, eax
            ; ucomisd xmm0, Rx(x)
            ; jne => not_int
            ; jp => not_int
            ; test eax, eax
            ; jnz => is_int
            ; movq rcx, Rx(x)
            ; test rcx, rcx
            ; js => not_int
            ; => is_int
            ; mov ecx, eax
            ; mov Rq(d), QWORD TAG_INT as i64
            ; or Rq(d), rcx
            ; jmp => done
            ; => not_int
            ; ucomisd Rx(x), Rx(x)
            ; jp => nan
            ; movq Rq(d), Rx(x)
            ; jmp => done
            ; => nan
            ; mov Rq(d), QWORD QNAN as i64
            ; => done
        );
    }

    fn inst(&mut self, v: V) {
        let f = self.f;
        let i = &f.insts[v as usize];
        let stub = i.fs.map(|s| self.stubs[s as usize]);
        if let Some(stub) = stub {
            self.stress_check(stub);
        }
        match i.op.clone() {
            Op::Phi(_) => {}
            Op::ConstI32(c) => {
                let (d, st) = self.dst_gpr(v);
                dynasm!(self.ops ; mov Rd(d), c);
                self.done_gpr(d, st);
            }
            Op::ConstBool(b) => {
                let (d, st) = self.dst_gpr(v);
                dynasm!(self.ops ; mov Rd(d), i32::from(b));
                self.done_gpr(d, st);
            }
            Op::ConstTagged(bits) => {
                let (d, st) = self.dst_gpr(v);
                dynasm!(self.ops ; mov Rq(d), QWORD bits as i64);
                self.done_gpr(d, st);
            }
            Op::ConstF64(x) => {
                let (d, st) = self.dst_xmm(v);
                if x.to_bits() == 0 {
                    dynasm!(self.ops ; xorps Rx(d), Rx(d));
                } else {
                    dynasm!(self.ops
                        ; mov rax, QWORD x.to_bits() as i64
                        ; movq Rx(d), rax
                    );
                }
                self.done_xmm(d, st);
            }
            Op::Param(r) => {
                let (d, st) = self.dst_gpr(v);
                let off = 8 * r as i32;
                dynasm!(self.ops ; mov Rq(d), [rbx + off]);
                self.done_gpr(d, st);
            }
            Op::I32ToF64(a) => {
                let ra = self.gpr(a, RDX);
                let (d, st) = self.dst_xmm(v);
                dynasm!(self.ops ; cvtsi2sd Rx(d), Rd(ra));
                self.done_xmm(d, st);
            }
            Op::TaggedToF64(a) => {
                let stub = stub.expect("a guard has a frame state");
                let ra = self.gpr(a, RDX);
                let (d, st) = self.dst_xmm(v);
                let is_int = self.ops.new_dynamic_label();
                let done = self.ops.new_dynamic_label();
                dynasm!(self.ops
                    ; mov rax, Rq(ra)
                    ; shr rax, 48
                    ; cmp eax, 0x7FF9
                    ; je => is_int
                    ; sub eax, 0x7FF9
                    ; cmp eax, 4
                    ; jbe => stub
                    ; movq Rx(d), Rq(ra)
                    ; jmp => done
                    ; => is_int
                    ; cvtsi2sd Rx(d), Rd(ra)
                    ; => done
                );
                self.done_xmm(d, st);
            }
            Op::F64ToI32(a) => {
                let stub = stub.expect("a guard has a frame state");
                let xa = self.xmm(a, X1);
                let (d, st) = self.dst_gpr(v);
                dynasm!(self.ops
                    ; cvttsd2si rax, Rx(xa)
                    ; mov rcx, QWORD SIGN as i64
                    ; cmp rax, rcx
                    ; je => stub
                    ; mov Rd(d), eax
                );
                self.done_gpr(d, st);
            }
            Op::Box(a) => {
                let (d, st) = self.dst_gpr(v);
                match self.rep(a) {
                    Rep::Tagged => {
                        let ra = self.gpr(a, RDX);
                        if ra != d {
                            dynasm!(self.ops ; mov Rq(d), Rq(ra));
                        }
                    }
                    r @ (Rep::I32 | Rep::Bool) => {
                        let ra = self.gpr(a, RDX);
                        let tag = if r == Rep::I32 { TAG_INT } else { TAG_BOOL };
                        dynasm!(self.ops
                            ; mov eax, Rd(ra)
                            ; mov Rq(d), QWORD tag as i64
                            ; or Rq(d), rax
                        );
                    }
                    Rep::F64 => {
                        let xa = self.xmm(a, X1);
                        self.box_f64(xa, d);
                    }
                    Rep::Void => unreachable!("a guard has no value to box"),
                }
                self.done_gpr(d, st);
            }
            Op::TaggedTruthy(a) => {
                let stub = stub.expect("a guard has a frame state");
                let ra = self.gpr(a, RDX);
                let (d, st) = self.dst_gpr(v);
                let (int, boolean, null_or_big, truthy, falsy, done) = (
                    self.ops.new_dynamic_label(),
                    self.ops.new_dynamic_label(),
                    self.ops.new_dynamic_label(),
                    self.ops.new_dynamic_label(),
                    self.ops.new_dynamic_label(),
                    self.ops.new_dynamic_label(),
                );
                dynasm!(self.ops
                    ; mov rax, Rq(ra)
                    ; shr rax, 48
                    ; cmp eax, 0x7FF9
                    ; je => int
                    ; cmp eax, 0x7FFA
                    ; je => boolean
                    ; cmp eax, 0x7FFC
                    ; je => falsy
                    ; cmp eax, 0x7FFB
                    ; je => null_or_big
                    ; cmp eax, 0x7FFD
                    ; je => stub
                    // A double: neither +-0 nor NaN.
                    ; mov rax, Rq(ra)
                    ; shl rax, 1
                    ; jz => falsy
                    ; mov rcx, QWORD ABS2_INF as i64
                    ; cmp rax, rcx
                    ; ja => falsy
                    ; jmp => truthy
                    ; => null_or_big
                    ; mov rax, QWORD TAG_NULL as i64
                    ; cmp Rq(ra), rax
                    ; je => falsy
                    ; jmp => stub
                    ; => int
                    ; test Rd(ra), Rd(ra)
                    ; jz => falsy
                    ; jmp => truthy
                    ; => boolean
                    ; mov eax, Rd(ra)
                    ; and eax, 1
                    ; jz => falsy
                    ; => truthy
                    ; mov Rd(d), 1
                    ; jmp => done
                    ; => falsy
                    ; mov Rd(d), 0
                    ; => done
                );
                self.done_gpr(d, st);
            }
            Op::F64Truthy(a) => {
                let xa = self.xmm(a, X1);
                let (d, st) = self.dst_gpr(v);
                let (falsy, done) = (self.ops.new_dynamic_label(), self.ops.new_dynamic_label());
                dynasm!(self.ops
                    ; movq rax, Rx(xa)
                    ; shl rax, 1
                    ; jz => falsy
                    ; mov rcx, QWORD ABS2_INF as i64
                    ; cmp rax, rcx
                    ; ja => falsy
                    ; mov Rd(d), 1
                    ; jmp => done
                    ; => falsy
                    ; mov Rd(d), 0
                    ; => done
                );
                self.done_gpr(d, st);
            }
            Op::I32Truthy(a) => {
                let ra = self.gpr(a, RDX);
                let (d, st) = self.dst_gpr(v);
                dynasm!(self.ops
                    ; xor eax, eax
                    ; test Rd(ra), Rd(ra)
                    ; setne al
                    ; mov Rd(d), eax
                );
                self.done_gpr(d, st);
            }
            Op::AddF(x, y) | Op::SubF(x, y) | Op::MulF(x, y) | Op::DivF(x, y) => {
                let xa = self.xmm(x, X1);
                let xb = self.xmm(y, X2);
                let (d, st) = self.dst_xmm(v);
                // The op into `t`, which holds `xa` first.
                let t = if d == xb && d != xa { 0 } else { d };
                if t != xa {
                    dynasm!(self.ops ; movapd Rx(t), Rx(xa));
                }
                match i.op {
                    Op::AddF(..) => dynasm!(self.ops ; addsd Rx(t), Rx(xb)),
                    Op::SubF(..) => dynasm!(self.ops ; subsd Rx(t), Rx(xb)),
                    Op::MulF(..) => dynasm!(self.ops ; mulsd Rx(t), Rx(xb)),
                    _ => dynasm!(self.ops ; divsd Rx(t), Rx(xb)),
                }
                if t != d {
                    dynasm!(self.ops ; movapd Rx(d), Rx(t));
                }
                self.done_xmm(d, st);
            }
            Op::NegF(x) => {
                let xa = self.xmm(x, X1);
                let (d, st) = self.dst_xmm(v);
                dynasm!(self.ops
                    ; mov rax, QWORD SIGN as i64
                    ; movq xmm0, rax
                );
                if d != xa {
                    dynasm!(self.ops ; movapd Rx(d), Rx(xa));
                }
                dynasm!(self.ops ; xorpd Rx(d), xmm0);
                self.done_xmm(d, st);
            }
            Op::Bit(op, x, y) => {
                let ra = self.gpr(x, RDX);
                let rb = self.gpr(y, R11);
                let (d, st) = self.dst_gpr(v);
                match op {
                    BitwiseOp::And => dynasm!(self.ops ; mov eax, Rd(ra) ; and eax, Rd(rb)),
                    BitwiseOp::Or => dynasm!(self.ops ; mov eax, Rd(ra) ; or eax, Rd(rb)),
                    BitwiseOp::Xor => dynasm!(self.ops ; mov eax, Rd(ra) ; xor eax, Rd(rb)),
                    BitwiseOp::Shl => dynasm!(self.ops ; mov ecx, Rd(rb) ; mov eax, Rd(ra) ; shl eax, cl),
                    BitwiseOp::Shr => dynasm!(self.ops ; mov ecx, Rd(rb) ; mov eax, Rd(ra) ; sar eax, cl),
                    BitwiseOp::Ushr => unreachable!("`>>>` builds as UshrF"),
                }
                dynasm!(self.ops ; mov Rd(d), eax);
                self.done_gpr(d, st);
            }
            Op::UshrF(x, y) => {
                let ra = self.gpr(x, RDX);
                let rb = self.gpr(y, R11);
                let (d, st) = self.dst_xmm(v);
                dynasm!(self.ops
                    ; mov ecx, Rd(rb)
                    ; mov eax, Rd(ra)
                    ; shr eax, cl
                    ; cvtsi2sd Rx(d), rax
                );
                self.done_xmm(d, st);
            }
            Op::CmpF(c, x, y) => {
                let xa = self.xmm(x, X1);
                let xb = self.xmm(y, X2);
                let (d, st) = self.dst_gpr(v);
                match c {
                    Cmp::Lt => dynasm!(self.ops ; ucomisd Rx(xb), Rx(xa) ; seta al),
                    Cmp::Le => dynasm!(self.ops ; ucomisd Rx(xb), Rx(xa) ; setae al),
                    Cmp::Gt => dynasm!(self.ops ; ucomisd Rx(xa), Rx(xb) ; seta al),
                    Cmp::Ge => dynasm!(self.ops ; ucomisd Rx(xa), Rx(xb) ; setae al),
                    Cmp::Eq => dynasm!(self.ops ; ucomisd Rx(xa), Rx(xb) ; sete al ; setnp cl ; and al, cl),
                    Cmp::Ne => dynasm!(self.ops ; ucomisd Rx(xa), Rx(xb) ; setne al ; setp cl ; or al, cl),
                }
                dynasm!(self.ops ; movzx eax, al ; mov Rd(d), eax);
                self.done_gpr(d, st);
            }
            Op::CmpB(eq, x, y) => {
                let ra = self.gpr(x, RDX);
                let rb = self.gpr(y, R11);
                let (d, st) = self.dst_gpr(v);
                dynasm!(self.ops ; mov eax, Rd(ra) ; cmp eax, Rd(rb));
                if eq {
                    dynasm!(self.ops ; sete al);
                } else {
                    dynasm!(self.ops ; setne al);
                }
                dynasm!(self.ops ; movzx eax, al ; mov Rd(d), eax);
                self.done_gpr(d, st);
            }
            Op::NotB(x) => {
                let ra = self.gpr(x, RDX);
                let (d, st) = self.dst_gpr(v);
                dynasm!(self.ops ; mov eax, Rd(ra) ; xor eax, 1 ; mov Rd(d), eax);
                self.done_gpr(d, st);
            }
            Op::StrictEqT(eq, x, y) => {
                let stub = stub.expect("a guard has a frame state");
                let ra = self.gpr(x, RDX);
                let rb = self.gpr(y, R11);
                let (d, st) = self.dst_gpr(v);
                let l: Vec<DynamicLabel> = (0..8).map(|_| self.ops.new_dynamic_label()).collect();
                let (differ, mixed, a_int, a_done, b_int, b_done, is_true, is_false) =
                    (l[0], l[1], l[2], l[3], l[4], l[5], l[6], l[7]);
                let done = self.ops.new_dynamic_label();
                dynasm!(self.ops
                    ; mov rax, Rq(ra)
                    ; mov rcx, Rq(rb)
                    ; cmp rax, rcx
                    ; jne => differ
                    // The same bits: equal, but for NaN.
                    ; mov rdx, QWORD QNAN as i64
                    ; cmp rax, rdx
                    ; je => is_false
                    ; jmp => is_true
                    ; => differ
                    ; mov rdx, rax
                    ; shr rdx, 48
                    ; mov r11, rcx
                    ; shr r11, 48
                    ; cmp edx, 0x7FFD
                    ; jne => mixed
                    ; cmp r11d, 0x7FFD
                    ; je => stub
                    // Equal only as two Numbers.
                    ; => mixed
                    ; cmp edx, 0x7FF9
                    ; je => a_int
                    ; sub edx, 0x7FF9
                    ; cmp edx, 4
                    ; jbe => is_false
                    ; movq xmm0, rax
                    ; jmp => a_done
                    ; => a_int
                    ; cvtsi2sd xmm0, eax
                    ; => a_done
                    ; cmp r11d, 0x7FF9
                    ; je => b_int
                    ; sub r11d, 0x7FF9
                    ; cmp r11d, 4
                    ; jbe => is_false
                    ; movq xmm1, rcx
                    ; jmp => b_done
                    ; => b_int
                    ; cvtsi2sd xmm1, ecx
                    ; => b_done
                    ; ucomisd xmm0, xmm1
                    ; jne => is_false
                    ; jp => is_false
                    ; => is_true
                    ; mov Rd(d), i32::from(eq)
                    ; jmp => done
                    ; => is_false
                    ; mov Rd(d), i32::from(!eq)
                    ; => done
                );
                self.done_gpr(d, st);
            }
            Op::LoadGlobal(idx) => {
                let stub = stub.expect("a guard has a frame state");
                let (d, st) = self.dst_gpr(v);
                let route = crate::vm::host_api::JIT_GLOBAL_ROUTE_EPOCH_OFFSET as i32;
                let base = crate::vm::host_api::JIT_GLOBALS_RAW_OFFSET as i32;
                let off = 8 * idx as i32;
                dynasm!(self.ops
                    // A global routed through the global object's own
                    // property, or one in its TDZ, is the interpreter's.
                    ; cmp DWORD [rdi + route], 0
                    ; jne => stub
                    ; mov rax, [rdi + base]
                    ; mov Rq(d), [rax + off]
                    ; mov rax, QWORD Value::UNINITIALIZED.bits() as i64
                    ; cmp Rq(d), rax
                    ; je => stub
                );
                self.done_gpr(d, st);
            }
            Op::GuardFid { v: x, fid } => {
                let stub = stub.expect("a guard has a frame state");
                let rx = self.gpr(x, RDX);
                let hot_off = crate::vm::host_api::JIT_HOT_MIRROR_RAW_OFFSET as i32;
                let hot_fid = crate::vm::host_api::JIT_HOT_FID_OFF as i32;
                dynasm!(self.ops
                    ; mov rax, Rq(rx)
                    ; shr rax, 48
                    ; cmp eax, 0x7FFD
                    ; jne => stub
                    ; mov ecx, Rd(rx)
                    ; mov r11, [rdi + hot_off]
                    ; lea r11, [r11 + rcx*8]
                    ; cmp DWORD [r11 + rcx*8 + hot_fid], fid as i32
                    ; jne => stub
                );
            }
            Op::GuardClass {
                obj,
                recv_shape,
                class,
                ver,
                epoch,
            } => {
                let stub = stub.expect("a guard has a frame state");
                let ro = self.gpr(obj, RDX);
                let hot_off = crate::vm::host_api::JIT_HOT_MIRROR_RAW_OFFSET as i32;
                let hot_fid = crate::vm::host_api::JIT_HOT_FID_OFF as i32;
                let versions = crate::vm::host_api::JIT_VERSIONS_RAW_OFFSET as i32;
                let epoch_off = crate::vm::host_api::JIT_CLASS_PROTO_EPOCH_OFFSET as i32;
                let class_word = crate::heap::class_mirror_fid(class);
                let class_ver = 4 * class as i32;
                dynasm!(self.ops
                    ; mov rax, Rq(ro)
                    ; shr rax, 48
                    ; cmp eax, 0x7FFD
                    ; jne => stub
                    ; mov ecx, Rd(ro)
                    ; mov r11, [rdi + hot_off]
                    ; lea r11, [r11 + rcx*8]
                    // No own property of the key at this shape...
                    ; cmp DWORD [r11 + rcx*8], recv_shape as i32
                    ; jne => stub
                    // ...an instance of the class...
                    ; cmp DWORD [r11 + rcx*8 + hot_fid], class_word as i32
                    ; jne => stub
                    // ...whose methods are as they were.
                    ; mov r11, [rdi + versions]
                    ; cmp DWORD [r11 + class_ver], ver as i32
                    ; jne => stub
                    ; cmp DWORD [rdi + epoch_off], epoch as i32
                    ; jne => stub
                );
            }
            Op::CallOut { kind, ip, packed, argc } => {
                let s = i.fs.expect("a call-out has a frame state");
                let exit = self.ops.new_dynamic_label();
                let done = self.ops.new_dynamic_label();
                let (d, st) = self.dst_gpr(v);
                self.write_state(s);
                let helper = match kind {
                    CallKind::Plain => crate::vm::jit_call_ic_cross as *const () as i64,
                    CallKind::Method => crate::vm::jit_call_method_ic as *const () as i64,
                    CallKind::WithThis => crate::vm::jit_call_with_this_cross as *const () as i64,
                };
                let fip = ((f.func_id as u64) << 32 | ip as u64) as i64;
                dynasm!(self.ops
                    ; mov rcx, rdi
                    ; mov rdx, rbx
                    ; mov r8, QWORD fip
                    ; mov r9, QWORD packed as i64
                    ; mov DWORD [rsp + 32], argc as i32
                    ; mov rax, QWORD helper
                    ; call rax
                    ; mov r10, QWORD crate::codegen::SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => exit
                    ; mov r10, QWORD crate::codegen::CALL_THREW as i64
                    ; cmp rax, r10
                    ; je => exit
                    ; mov Rq(d), rax
                    ; jmp => done
                    // Declined (the interpreter makes the call) or threw (it
                    // unwinds): the window already holds the frame state.
                    ; => exit
                    ; mov DWORD [rsi], ip as i32
                    ; xor eax, eax
                    ; jmp => self.epilogue
                    ; => done
                );
                self.done_gpr(d, st);
            }
            Op::GetPropShape { obj, shape, slot } => {
                let stub = stub.expect("a guard has a frame state");
                let ro = self.gpr(obj, RDX);
                let (d, st) = self.dst_gpr(v);
                let hot_off = crate::vm::host_api::JIT_HOT_MIRROR_RAW_OFFSET as i32;
                let hot_vals = crate::vm::host_api::JIT_HOT_VALS_OFF as i32;
                let off = 8 * slot as i32;
                dynasm!(self.ops
                    ; mov rax, Rq(ro)
                    ; shr rax, 48
                    ; cmp eax, 0x7FFD
                    ; jne => stub
                    ; mov ecx, Rd(ro)
                    ; mov r11, [rdi + hot_off]
                    ; lea r11, [r11 + rcx*8]
                    ; cmp DWORD [r11 + rcx*8], shape as i32
                    ; jne => stub
                    ; mov r11, [r11 + rcx*8 + hot_vals]
                    ; mov Rq(d), [r11 + off]
                );
                self.done_gpr(d, st);
            }
        }
    }

    /// One move between locations of the same value class.
    fn mov(&mut self, s: Loc, d: Loc) {
        match (s, d) {
            (Loc::Gpr(a), Loc::Gpr(b)) => dynasm!(self.ops ; mov Rq(b), Rq(a)),
            (Loc::Xmm(a), Loc::Xmm(b)) => dynasm!(self.ops ; movapd Rx(b), Rx(a)),
            (Loc::Gpr(a), Loc::Xmm(b)) => dynasm!(self.ops ; movq Rx(b), Rq(a)),
            (Loc::Xmm(a), Loc::Gpr(b)) => dynasm!(self.ops ; movq Rq(b), Rx(a)),
            (Loc::Gpr(a), Loc::Slot(t)) => {
                let o = self.slot(t);
                dynasm!(self.ops ; mov [rsp + o], Rq(a));
            }
            (Loc::Xmm(a), Loc::Slot(t)) => {
                let o = self.slot(t);
                dynasm!(self.ops ; movsd QWORD [rsp + o], Rx(a));
            }
            (Loc::Slot(t), Loc::Gpr(b)) => {
                let o = self.slot(t);
                dynasm!(self.ops ; mov Rq(b), [rsp + o]);
            }
            (Loc::Slot(t), Loc::Xmm(b)) => {
                let o = self.slot(t);
                dynasm!(self.ops ; movsd Rx(b), QWORD [rsp + o]);
            }
            (Loc::Slot(t), Loc::Slot(u)) => {
                let (o, p) = (self.slot(t), self.slot(u));
                dynasm!(self.ops ; mov rcx, [rsp + o] ; mov [rsp + p], rcx);
            }
        }
    }

    /// The phi moves of the edge `from` -> `to`, as one parallel move.
    fn edge_moves(&mut self, from: usize, to: usize) {
        let tb = &self.f.blocks[to];
        let Some(k) = tb.preds.iter().position(|&p| p as usize == from) else {
            return;
        };
        let mut pending: Vec<(Loc, Loc)> = Vec::new();
        for &phi in &tb.insts {
            if let Op::Phi(ins) = &self.f.insts[phi as usize].op {
                let (s, d) = (self.a.loc[ins[k] as usize], self.a.loc[phi as usize]);
                if s != d {
                    pending.push((s, d));
                }
            }
        }
        while !pending.is_empty() {
            if let Some(i) = pending.iter().position(|&(_, d)| !pending.iter().any(|&(s, _)| s == d)) {
                let (s, d) = pending.remove(i);
                self.mov(s, d);
            } else {
                // A cycle: park one source in a temporary.
                let (s, _) = pending[0];
                let temp = if pending.iter().any(|&(a, b)| matches!(a, Loc::Xmm(_)) || matches!(b, Loc::Xmm(_))) {
                    Loc::Xmm(0)
                } else {
                    Loc::Gpr(RAX)
                };
                self.mov(s, temp);
                for m in pending.iter_mut() {
                    if m.0 == s {
                        m.0 = temp;
                    }
                }
            }
        }
    }
}

pub(crate) fn emit(f: &Func, a: &Alloc) -> Option<JitFn> {
    let mut ops = Asm::new().ok()?;
    let block_labels: Vec<DynamicLabel> = f.blocks.iter().map(|_| ops.new_dynamic_label()).collect();
    let stubs: Vec<DynamicLabel> = f.states.iter().map(|_| ops.new_dynamic_label()).collect();
    let epilogue = ops.new_dynamic_label();
    // The callee-saved registers the allocation used.
    let mut gpr_saves: Vec<u8> = Vec::new();
    let mut xmm_saves: Vec<u8> = Vec::new();
    for l in &a.loc {
        match *l {
            Loc::Gpr(r) if [5, 12, 13, 14, 15].contains(&r) && !gpr_saves.contains(&r) => gpr_saves.push(r),
            Loc::Xmm(r) if r >= 6 && !xmm_saves.contains(&r) => xmm_saves.push(r),
            _ => {}
        }
    }
    gpr_saves.sort_unstable();
    xmm_saves.sort_unstable();
    let pushes = 3 + gpr_saves.len() as i32;
    let xmm_area = 16 * xmm_saves.len() as i32;
    let mut frame = OUT_AREA + xmm_area + 8 * a.slots as i32;
    if (8 + 8 * pushes + frame) % 16 != 0 {
        frame += 8;
    }
    dynasm!(ops
        ; push rbx
        ; push rsi
        ; push rdi
    );
    for &r in &gpr_saves {
        dynasm!(ops ; push Rq(r));
    }
    dynasm!(ops ; sub rsp, frame);
    for (k, &x) in xmm_saves.iter().enumerate() {
        let off = OUT_AREA + 16 * k as i32;
        dynasm!(ops ; movdqu [rsp + off], Rx(x));
    }
    dynasm!(ops
        ; mov rbx, rcx
        ; mov rsi, rdx
        ; mov rdi, r8
    );
    let stress = super::stress_every() != 0;
    // Each value's readers, to fuse a compare into the branch that is its
    // only reader (no boolean made, then tested).
    let mut uses = vec![0u32; f.insts.len()];
    for inst in &f.insts {
        for u in inst.op.operands() {
            uses[u as usize] += 1;
        }
        if let Op::Phi(ins) = &inst.op {
            for &u in ins {
                uses[u as usize] += 1;
            }
        }
        if let Some(s) = inst.fs {
            for &(_, x) in &f.states[s as usize].regs {
                uses[x as usize] += 1;
            }
        }
    }
    for b in &f.blocks {
        match b.term {
            Term::Branch { cond, .. } => uses[cond as usize] += 1,
            Term::Return(v) => uses[v as usize] += 1,
            Term::Jump(_) => {}
        }
    }
    let fused = |b: &super::build::Block| -> Option<V> {
        let Term::Branch { cond, .. } = b.term else {
            return None;
        };
        (uses[cond as usize] == 1
            && b.insts.last() == Some(&cond)
            && matches!(f.insts[cond as usize].op, Op::CmpF(..)))
        .then_some(cond)
    };
    let mut em = Em {
        ops,
        f,
        a,
        slot_base: OUT_AREA + xmm_area,
        stubs,
        stress,
        epilogue,
    };
    for (bi, b) in f.blocks.iter().enumerate() {
        dynasm!(em.ops ; => block_labels[bi]);
        let fuse = fused(b);
        for &v in &b.insts {
            if Some(v) != fuse {
                em.inst(v);
            }
        }
        if let (Some(c), Term::Branch { t, f: fb, .. }) = (fuse, &b.term) {
            let Op::CmpF(cmp, x, y) = f.insts[c as usize].op else {
                unreachable!("only a Number compare fuses");
            };
            let xa = em.xmm(x, X1);
            let xb = em.xmm(y, X2);
            let lf = em.ops.new_dynamic_label();
            let lt = em.ops.new_dynamic_label();
            match cmp {
                Cmp::Lt => dynasm!(em.ops ; ucomisd Rx(xb), Rx(xa) ; jbe => lf),
                Cmp::Le => dynasm!(em.ops ; ucomisd Rx(xb), Rx(xa) ; jb => lf),
                Cmp::Gt => dynasm!(em.ops ; ucomisd Rx(xa), Rx(xb) ; jbe => lf),
                Cmp::Ge => dynasm!(em.ops ; ucomisd Rx(xa), Rx(xb) ; jb => lf),
                Cmp::Eq => dynasm!(em.ops ; ucomisd Rx(xa), Rx(xb) ; jne => lf ; jp => lf),
                Cmp::Ne => dynasm!(em.ops ; ucomisd Rx(xa), Rx(xb) ; jp => lt ; je => lf),
            }
            dynasm!(em.ops ; => lt);
            em.edge_moves(bi, *t as usize);
            dynasm!(em.ops ; jmp => block_labels[*t as usize] ; => lf);
            em.edge_moves(bi, *fb as usize);
            if *fb as usize != bi + 1 {
                dynasm!(em.ops ; jmp => block_labels[*fb as usize]);
            }
            continue;
        }
        match b.term {
            Term::Jump(s) => {
                em.edge_moves(bi, s as usize);
                if s as usize != bi + 1 {
                    dynasm!(em.ops ; jmp => block_labels[s as usize]);
                }
            }
            Term::Branch { cond, t, f: fb } => {
                let rc = em.gpr(cond, RDX);
                let lf = em.ops.new_dynamic_label();
                dynasm!(em.ops
                    ; test Rd(rc), Rd(rc)
                    ; jz => lf
                );
                em.edge_moves(bi, t as usize);
                dynasm!(em.ops ; jmp => block_labels[t as usize] ; => lf);
                em.edge_moves(bi, fb as usize);
                if fb as usize != bi + 1 {
                    dynasm!(em.ops ; jmp => block_labels[fb as usize]);
                }
            }
            Term::Return(v) => {
                let rv = em.gpr(v, RDX);
                dynasm!(em.ops
                    ; mov rax, Rq(rv)
                    ; mov DWORD [rsi], crate::codegen::NO_BAIL as i32
                    ; jmp => epilogue
                );
            }
        }
    }
    // ── deopt stubs ──
    let helper = crate::vm::jit_opt_deopt as *const () as i64;
    for (k, s) in f.states.iter().enumerate() {
        dynasm!(em.ops ; => em.stubs[k]);
        em.write_state(k as u32);
        dynasm!(em.ops
            ; mov DWORD [rsi], s.ip as i32
            ; mov rcx, rdi
            ; mov edx, f.func_id as i32
            ; mov r8d, s.ip as i32
            ; mov rax, QWORD helper
            ; call rax
            ; xor eax, eax
            ; jmp => epilogue
        );
    }
    // ── epilogue ──
    dynasm!(em.ops ; => epilogue);
    for (k, &x) in xmm_saves.iter().enumerate() {
        let off = OUT_AREA + 16 * k as i32;
        dynasm!(em.ops ; movdqu Rx(x), [rsp + off]);
    }
    dynasm!(em.ops ; add rsp, frame);
    for &r in gpr_saves.iter().rev() {
        dynasm!(em.ops ; pop Rq(r));
    }
    dynasm!(em.ops
        ; pop rdi
        ; pop rsi
        ; pop rbx
        ; ret
    );
    let buf = em.ops.finalize().ok()?;
    let entry = buf.ptr(dynasmrt::AssemblyOffset(0));
    // `ZIPP_OPT_DUMP=<dir>`: the body's bytes, for a disassembler.
    if let Some(dir) = std::env::var_os("ZIPP_OPT_DUMP") {
        let path = std::path::Path::new(&dir).join(format!("opt_fn{}.bin", f.func_id));
        let _ = std::fs::write(path, &buf[..]);
    }
    Some(JitFn {
        _buf: buf,
        entry,
        self_binding: None,
        int_tier: false,
        osr_heads: Box::new([]),
    })
}
