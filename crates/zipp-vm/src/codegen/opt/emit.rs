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
use super::build::{CallLane, Cmp, Func, MethodIntr, Op, Rep, Term, V};
use crate::bytecode::{BitwiseOp, MathFn};
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
    /// The frame offset of the first typed-array record (`Func::ta_records`,
    /// 40 bytes each), and Tier C's element stubs that read and write
    /// through one.
    ta_base: i32,
    ta_read: DynamicLabel,
    ta_write: DynamicLabel,
    /// The 64-byte scratch of Tier C's cross-call lanes (`emit_cross3_invoke`).
    c3: i32,
}

/// The cross-call lanes' scratch bytes.
const C3_SCRATCH: i32 = 64;

/// A typed-array record's bytes (`jit_get_index_ta_fill`).
const TA_RECORD: i32 = 40;

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

    /// The op at `ip` through `helper`, of the call protocol, with frame
    /// state `s` written into the window first; its result in rax. A helper
    /// that declined (the interpreter makes the op) or threw (it unwinds)
    /// leaves the body at `ip`, the window already holding the state.
    fn call_out(&mut self, s: u32, helper: u64, ip: u32, packed: u64, argc: u16) {
        self.write_state(s);
        self.call_written(helper, ip, packed, argc);
    }

    /// Tier C's emitted cross-call lanes for the call at `ip` (`CallLane`,
    /// `Func::cross`), the frame state already in the window they read:
    /// a lane that makes the call leaves its result in the window's `dst`
    /// register and jumps to the returned label; every miss falls through to
    /// the call-out's helper after them. A throw leaves the body at `ip`.
    fn cross_lanes(&mut self, lane: CallLane, ip: u32) -> Option<(DynamicLabel, u16)> {
        use super::super::{emit_cross3_call, emit_cross3_method_call, Cross3Callee};
        let f = self.f;
        let site = *f.cross.get(&ip)?;
        let heap = f.heap.as_ref()?;
        let arms = !site.cross3.is_empty();
        let own = site.cross3m.filter(|_| super::super::tierc_cross3m_enabled());
        let dst = match lane {
            CallLane::None => return None,
            CallLane::Plain { dst, .. } if arms => dst,
            CallLane::Method { dst, .. } if own.is_some() || (site.class_method && arms) => dst,
            _ => return None,
        };
        let (cross_done, bail, over) = (
            self.ops.new_dynamic_label(),
            self.ops.new_dynamic_label(),
            self.ops.new_dynamic_label(),
        );
        let epilogue = self.epilogue;
        dynasm!(self.ops
            ; jmp => over
            ; => bail
            ; mov DWORD [rsi], ip as i32
            ; xor eax, eax
            ; jmp => epilogue
            ; => over
        );
        let (regs, c3) = (f.reg_count, self.c3);
        match lane {
            CallLane::Plain {
                callee,
                this,
                arg_base,
                dst,
            } => {
                for arm in site.cross3.iter() {
                    emit_cross3_call(
                        &mut self.ops,
                        arm,
                        Cross3Callee::Reg(callee),
                        this,
                        arg_base,
                        dst,
                        regs,
                        c3,
                        heap,
                        bail,
                        cross_done,
                        false,
                        None,
                    );
                }
            }
            CallLane::Method { obj, arg_base, dst, .. } => {
                if let Some(mp) = own {
                    emit_cross3_method_call(
                        &mut self.ops,
                        mp,
                        obj,
                        arg_base,
                        dst,
                        regs,
                        c3,
                        heap,
                        bail,
                        cross_done,
                        false,
                        None,
                    );
                }
                if site.class_method && arms {
                    // The method the receiver's class has, looked up and
                    // not called, then called by the arm naming its fid.
                    let lookup_miss = self.ops.new_dynamic_label();
                    let fip = ((f.func_id as u64) << 32 | ip as u64) as i64;
                    dynasm!(self.ops
                        ; mov rcx, rdi
                        ; mov rdx, [rbx + 8 * obj as i32]
                        ; mov r8, QWORD fip
                        ; mov rax, QWORD crate::vm::jit_method_callee as *const () as i64
                        ; call rax
                        ; mov r10, QWORD crate::codegen::SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; je => lookup_miss
                        ; mov [rsp + c3 + 48], rax
                    );
                    for arm in site.cross3.iter() {
                        emit_cross3_call(
                            &mut self.ops,
                            arm,
                            Cross3Callee::Stash,
                            Some(obj),
                            arg_base,
                            dst,
                            regs,
                            c3,
                            heap,
                            bail,
                            cross_done,
                            false,
                            None,
                        );
                    }
                    dynasm!(self.ops ; => lookup_miss);
                }
            }
            CallLane::None | CallLane::Prop { .. } => unreachable!(),
        }
        Some((cross_done, dst))
    }

    /// A slow property read's inline cache probe, Tier C's (`emit_ic_probe`
    /// and its miss helper, which fills the site), over the window the frame
    /// state fills: a hit or a filled miss stores the value to the window's
    /// `dst` register and jumps to the returned label; anything else (an
    /// accessor, a receiver the cache does not take) falls through to the
    /// read's helper. r13 and r14 hold the cache's bases meanwhile, saved in
    /// the scratch.
    fn prop_ic(&mut self, obj: u16, dst: u16, name: u32, ip: u32) -> Option<DynamicLabel> {
        use super::super::{emit_ic_probe, IcProbe, JIT_IC_STRIDE, JIT_IC_WAYS, PROP_VIA_IC};
        let (site, direct) = *self.f.ic_sites.get(&ip)?;
        let heap = self.f.heap?;
        let (done, hit, decline) = (
            self.ops.new_dynamic_label(),
            self.ops.new_dynamic_label(),
            self.ops.new_dynamic_label(),
        );
        let off = (site as usize * JIT_IC_WAYS * JIT_IC_STRIDE) as i32;
        let packed = ((self.f.func_id as u64) << 32) | name as u64;
        let c3 = self.c3;
        dynasm!(self.ops
            ; mov [rsp + c3], r13
            ; mov [rsp + c3 + 8], r14
            ; mov r13, [rdi + crate::vm::host_api::JIT_VERSIONS_RAW_OFFSET as i32]
            ; mov r14, [rdi + crate::vm::host_api::JIT_IC_TABLE_RAW_OFFSET as i32]
        );
        emit_ic_probe(&mut self.ops, IcProbe::Get { dst }, obj, off, hit, None, direct);
        dynasm!(self.ops
            ; mov rcx, rdi
            ; mov rdx, rax                        // the receiver, as the probe left it
            ; mov r8d, site as i32
            ; mov r9, QWORD packed as i64
            ; mov rax, QWORD heap.get_prop_miss as i64
            ; call rax
            ; mov r10, QWORD crate::codegen::SELF_CALL_DEOPT as i64
            ; cmp rax, r10
            ; je => decline
            ; mov r10, QWORD PROP_VIA_IC as i64
            ; cmp rax, r10
            ; je => decline
            ; mov [rbx + 8 * dst as i32], rax
            ; => hit
            ; mov r13, [rsp + c3]
            ; mov r14, [rsp + c3 + 8]
            ; jmp => done
            ; => decline
            ; mov r13, [rsp + c3]
            ; mov r14, [rsp + c3 + 8]
        );
        Some(done)
    }

    /// `obj.<method>(args)` through Tier C's dedicated helper for it (`m`),
    /// over the window the frame state fills: an answer is stored to the
    /// window's `dst` register and jumps to the returned label; a decline
    /// (another receiver, a user method of the name) falls through, nothing
    /// done. `None` when the helpers are not at hand.
    fn method_intr(&mut self, m: MethodIntr, obj: u16, arg_base: u16, dst: u16) -> Option<DynamicLabel> {
        let heap = self.f.heap?;
        let done = self.ops.new_dynamic_label();
        let decline = self.ops.new_dynamic_label();
        let (o, a0, d) = (8 * obj as i32, 8 * arg_base as i32, 8 * dst as i32);
        let deopt = crate::codegen::SELF_CALL_DEOPT as i64;
        // The helper's answer, or the decline.
        let answer = |em: &mut Self| {
            dynasm!(em.ops
                ; mov r10, QWORD deopt
                ; cmp rax, r10
                ; je => decline
                ; mov [rbx + d], rax
                ; jmp => done
            );
        };
        match m {
            MethodIntr::CharCodeAt | MethodIntr::IndexOf | MethodIntr::Push => {
                let helper = match m {
                    MethodIntr::IndexOf => heap.str_index_of,
                    MethodIntr::Push => heap.array_push,
                    _ => heap.char_code_at,
                };
                dynasm!(self.ops
                    ; mov rcx, rdi
                    ; mov rdx, [rbx + o]
                    ; mov r8, [rbx + a0]
                    ; mov rax, QWORD helper as i64
                    ; call rax
                );
                answer(self);
            }
            MethodIntr::Substring(mode) => {
                dynasm!(self.ops
                    ; mov rcx, rdi
                    ; mov rdx, [rbx + o]
                    ; lea r8, [rbx + a0]
                    ; mov r9d, mode
                    ; mov rax, QWORD heap.str_substring as i64
                    ; call rax
                );
                answer(self);
            }
            MethodIntr::Get | MethodIntr::Has => {
                // `has` asks as a Set (op 2) when not a Map's.
                let ops: &[i32] = if matches!(m, MethodIntr::Get) { &[0] } else { &[1, 2] };
                for &op in ops {
                    let next = self.ops.new_dynamic_label();
                    dynasm!(self.ops
                        ; mov rcx, rdi
                        ; mov rdx, [rbx + o]
                        ; mov r8, [rbx + a0]
                        ; mov r9d, op
                        ; mov rax, QWORD heap.coll_lookup as i64
                        ; call rax
                        ; mov r10, QWORD deopt
                        ; cmp rax, r10
                        ; je => next
                        ; mov [rbx + d], rax
                        ; jmp => done
                        ; => next
                    );
                }
                dynasm!(self.ops ; jmp => decline);
            }
            MethodIntr::Mutate(op, argc) => {
                // `delete` asks as a Set (op 4) when not a Map's.
                let ops: &[i32] = if op == 3 { &[3, 4] } else { &[op] };
                for &op in ops {
                    let next = self.ops.new_dynamic_label();
                    match argc {
                        2 => dynasm!(self.ops ; mov r8, [rbx + a0] ; mov r9, [rbx + a0 + 8]),
                        1 => dynasm!(self.ops ; mov r8, [rbx + a0] ; xor r9d, r9d),
                        _ => dynasm!(self.ops ; xor r8d, r8d ; xor r9d, r9d),
                    }
                    dynasm!(self.ops
                        ; mov QWORD [rsp + 32], op
                        ; mov rcx, rdi
                        ; mov rdx, [rbx + o]
                        ; mov rax, QWORD heap.coll_mutate as i64
                        ; call rax
                        ; mov r10, QWORD deopt
                        ; cmp rax, r10
                        ; je => next
                        ; mov [rbx + d], rax
                        ; jmp => done
                        ; => next
                    );
                }
                dynasm!(self.ops ; jmp => decline);
            }
            MethodIntr::Upper | MethodIntr::Pop => {
                let helper = match m {
                    MethodIntr::Upper => heap.str_upper_case as i64,
                    _ => crate::vm::jit_array_pop as *const () as i64,
                };
                dynasm!(self.ops
                    ; mov rcx, rdi
                    ; mov rdx, [rbx + o]
                    ; mov rax, QWORD helper
                    ; call rax
                );
                answer(self);
            }
        }
        dynasm!(self.ops ; => decline);
        Some(done)
    }

    /// `call_out` with the frame state already in the window.
    fn call_written(&mut self, helper: u64, ip: u32, packed: u64, argc: u16) {
        let fip = ((self.f.func_id as u64) << 32 | ip as u64) as i64;
        dynasm!(self.ops
            ; mov rcx, rdi
            ; mov rdx, rbx
            ; mov r8, QWORD fip
            ; mov r9, QWORD packed as i64
            ; mov DWORD [rsp + 32], argc as i32
            ; mov rax, QWORD helper as i64
            ; call rax
        );
        self.leave_on_sentinel(ip);
    }

    /// After a helper of the call protocol (the frame state already in the
    /// window): a declined op or a throw leaves the body at `ip`.
    fn leave_on_sentinel(&mut self, ip: u32) {
        let exit = self.ops.new_dynamic_label();
        let done = self.ops.new_dynamic_label();
        let epilogue = self.epilogue;
        dynasm!(self.ops
            ; mov r10, QWORD crate::codegen::SELF_CALL_DEOPT as i64
            ; cmp rax, r10
            ; je => exit
            ; mov r10, QWORD crate::codegen::CALL_THREW as i64
            ; cmp rax, r10
            ; jne => done
            ; => exit
            ; mov DWORD [rsi], ip as i32
            ; xor eax, eax
            ; jmp => epilogue
            ; => done
        );
    }

    /// Out at the ip in eax that a `finally` routing named, counted
    /// (`vm::jit_opt_route_exit`): the frame state is in the window.
    fn route_exit(&mut self) {
        let epilogue = self.epilogue;
        dynasm!(self.ops
            ; mov [rsp + 40], rax
            ; mov rcx, rdi
            ; mov edx, self.f.func_id as i32
            ; mov r8d, self.f.body_gen as i32
            ; mov rax, QWORD crate::vm::jit_opt_route_exit as *const () as i64
            ; call rax
            ; mov rax, [rsp + 40]
            ; mov DWORD [rsi], eax
            ; xor eax, eax
            ; jmp => epilogue
        );
    }

    /// A deopt at frame state `s` already written into the window: counted
    /// (`vm::jit_opt_deopt`), then out at its position.
    fn leave_counted(&mut self, s: u32) {
        let st = &self.f.states[s as usize];
        let (ip, why) = (st.ip, st.why);
        let epilogue = self.epilogue;
        dynasm!(self.ops
            ; mov DWORD [rsi], ip as i32
            ; mov rcx, rdi
            ; mov edx, self.f.func_id as i32
            ; mov r8d, why as i32
            ; mov r9d, self.f.body_gen as i32
            ; mov rax, QWORD crate::vm::jit_opt_deopt as *const () as i64
            ; call rax
            ; xor eax, eax
            ; jmp => epilogue
        );
    }

    /// The element index `key` names, zero-extended into rax (a negative one
    /// is huge, past every bound); anything but a whole Number to `slow`.
    /// Uses rax and xmm0 (and r11 for a spilled key).
    fn index_to_rax(&mut self, key: V, slow: DynamicLabel) {
        match self.rep(key) {
            Rep::I32 => {
                let rk = self.gpr(key, R11);
                dynasm!(self.ops ; mov eax, Rd(rk));
            }
            Rep::F64 => {
                let xk = self.xmm(key, X1);
                dynasm!(self.ops
                    ; cvttsd2si rax, Rx(xk)
                    ; cvtsi2sd xmm0, rax
                    ; ucomisd xmm0, Rx(xk)
                    ; jne => slow
                    ; jp => slow
                    ; mov eax, eax
                );
            }
            _ => {
                let rk = self.gpr(key, R11);
                dynasm!(self.ops
                    ; mov rax, Rq(rk)
                    ; shr rax, 48
                    ; cmp eax, 0x7FF9
                    ; jne => slow
                    ; mov eax, Rd(rk)
                );
            }
        }
    }

    /// The array mirror's entry of the object in `ro` (`Heap::arr_mirror`)
    /// into r10, the index in rax below its length; anything else to `slow`.
    /// Uses rcx and r10.
    fn mirror_entry(&mut self, ro: u8, slow: DynamicLabel) {
        let arr_raw = crate::vm::host_api::JIT_ARR_MIRROR_RAW_OFFSET as i32;
        let arr_len = crate::vm::host_api::JIT_ARR_MIRROR_LEN_OFFSET as i32;
        dynasm!(self.ops
            ; mov rcx, Rq(ro)
            ; shr rcx, 48
            ; cmp ecx, 0x7FFD
            ; jne => slow
            ; mov ecx, Rd(ro)
            ; cmp ecx, [rdi + arr_len]
            ; jae => slow
            ; mov r10, [rdi + arr_raw]
            ; shl rcx, 4
            ; add r10, rcx
            ; cmp rax, [r10 + 8]
            ; jae => slow
            ; mov r10, [r10]
        );
    }

    /// The typed-array record at `[rsp + rec]` still answers for the
    /// receiver in `ro` (`emit_ta_cache_get`'s guards: the same object, its
    /// slot not reused, no buffer moved since), its element count in r11;
    /// another receiver to `fill`, a record the helper declined to `slow`.
    /// Uses rcx, r10 and r11.
    fn ta_record_check(&mut self, ro: u8, rec: i32, fill: DynamicLabel, slow: DynamicLabel) {
        let epoch = &crate::heap::TA_DATA_EPOCH as *const _ as i64;
        let versions = crate::vm::host_api::JIT_VERSIONS_RAW_OFFSET as i32;
        dynasm!(self.ops
            ; cmp Rq(ro), [rsp + rec]
            ; jne => fill
            ; mov r11, [rsp + rec + 16]
            ; test r11, r11
            ; jz => slow
            ; mov r10, QWORD epoch
            ; mov r10, [r10]
            ; cmp r10, [rsp + rec + 32]
            ; jne => fill
            ; mov r10, [rdi + versions]
            ; mov ecx, Rd(ro)
            ; mov r10d, [r10 + rcx * 4]
            ; cmp r10d, [rsp + rec + 28]
            ; jne => fill
        );
    }

    /// `key` boxed into r8, for a helper. Uses rax, rcx and xmm0.
    fn key_to_r8(&mut self, key: V) {
        match self.rep(key) {
            Rep::I32 => {
                let rk = self.gpr(key, R11);
                dynasm!(self.ops
                    ; mov r8d, Rd(rk)
                    ; mov rax, QWORD TAG_INT as i64
                    ; or r8, rax
                );
            }
            Rep::F64 => {
                let xk = self.xmm(key, X1);
                self.box_f64(xk, 8);
            }
            _ => {
                let rk = self.gpr(key, R11);
                dynasm!(self.ops ; mov r8, Rq(rk));
            }
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
                Rep::Void | Rep::Raw => unreachable!("a frame state names values"),
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
            Op::TaggedToI32(a) => {
                let stub = stub.expect("a guard has a frame state");
                let ra = self.gpr(a, RDX);
                let (d, st) = self.dst_gpr(v);
                let dbl = self.ops.new_dynamic_label();
                let done = self.ops.new_dynamic_label();
                dynasm!(self.ops
                    ; mov rax, Rq(ra)
                    ; shr rax, 48
                    ; cmp eax, 0x7FF9
                    ; jne => dbl
                    ; mov Rd(d), Rd(ra)
                    ; jmp => done
                    ; => dbl
                    ; sub eax, 0x7FF9
                    ; cmp eax, 4
                    ; jbe => stub
                    ; movq xmm0, Rq(ra)
                    ; cvttsd2si rax, xmm0
                    ; mov rcx, QWORD SIGN as i64
                    ; cmp rax, rcx
                    ; je => stub
                    ; mov Rd(d), eax
                    ; => done
                );
                self.done_gpr(d, st);
            }
            Op::AddI(a, b, wrap) | Op::SubI(a, b, wrap) => {
                let stub = stub.expect("a guard has a frame state");
                let ra = self.gpr(a, RDX);
                let rb = self.gpr(b, R11);
                let (d, st) = self.dst_gpr(v);
                dynasm!(self.ops ; mov eax, Rd(ra));
                if matches!(i.op, Op::AddI(..)) {
                    dynasm!(self.ops ; add eax, Rd(rb));
                } else {
                    dynasm!(self.ops ; sub eax, Rd(rb));
                }
                if !wrap {
                    dynasm!(self.ops ; jo => stub);
                }
                dynasm!(self.ops ; mov Rd(d), eax);
                self.done_gpr(d, st);
            }
            Op::AddIK(a, k, wrap) => {
                let stub = stub.expect("a guard has a frame state");
                let ra = self.gpr(a, RDX);
                let (d, st) = self.dst_gpr(v);
                dynasm!(self.ops
                    ; mov eax, Rd(ra)
                    ; add eax, k
                );
                if !wrap {
                    dynasm!(self.ops ; jo => stub);
                }
                dynasm!(self.ops ; mov Rd(d), eax);
                self.done_gpr(d, st);
            }
            Op::UshrI(a, k) => {
                let ra = self.gpr(a, RDX);
                let (d, st) = self.dst_gpr(v);
                dynasm!(self.ops
                    ; mov eax, Rd(ra)
                    ; shr eax, k as i8
                    ; mov Rd(d), eax
                );
                self.done_gpr(d, st);
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
                    Rep::Void | Rep::Raw => unreachable!("a guard or a pointer has no value to box"),
                }
                self.done_gpr(d, st);
            }
            Op::TaggedTruthy(a) => {
                let ra = self.gpr(a, RDX);
                let (d, st) = self.dst_gpr(v);
                let heap = self.ops.new_dynamic_label();
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
                    ; je => heap
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
                    // A heap value (an empty string, a zero BigInt and the
                    // [[IsHTMLDDA]] object are falsy): `jit_truthy`, read-only,
                    // with the volatile registers the allocator hands out
                    // kept, so the test is no call position.
                    ; => heap
                    ; mov rdx, Rq(ra)
                    ; sub rsp, 80
                    ; mov [rsp + 32], r8
                    ; mov [rsp + 40], r9
                    ; movdqu [rsp + 48], xmm4
                    ; movdqu [rsp + 64], xmm5
                    ; mov rcx, rdi
                    ; mov rax, QWORD crate::vm::jit_truthy as *const () as i64
                    ; call rax
                    ; mov r8, [rsp + 32]
                    ; mov r9, [rsp + 40]
                    ; movdqu xmm4, [rsp + 48]
                    ; movdqu xmm5, [rsp + 64]
                    ; add rsp, 80
                    ; test eax, eax
                    ; jz => falsy
                    ; jmp => truthy
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
            Op::CmpI(c, x, y) => {
                let ra = self.gpr(x, RDX);
                let rb = self.gpr(y, R11);
                let (d, st) = self.dst_gpr(v);
                dynasm!(self.ops ; cmp Rd(ra), Rd(rb));
                match c {
                    Cmp::Lt => dynasm!(self.ops ; setl al),
                    Cmp::Le => dynasm!(self.ops ; setle al),
                    Cmp::Gt => dynasm!(self.ops ; setg al),
                    Cmp::Ge => dynasm!(self.ops ; setge al),
                    Cmp::Eq => dynasm!(self.ops ; sete al),
                    Cmp::Ne => dynasm!(self.ops ; setne al),
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
                let base = crate::vm::host_api::JIT_GLOBALS_RAW_OFFSET as i32;
                let off = 8 * idx as i32;
                dynasm!(self.ops
                    // The slot, as Tier C reads it: a body is entered only
                    // while its globals' routes hold (`jit_globals_still_
                    // routable`), and one that changed since then is the
                    // interpreter's (`Vm::jit_route_dirty`). One in its TDZ
                    // is the interpreter's too.
                    ; cmp BYTE [rdi + crate::vm::host_api::JIT_ROUTE_DIRTY_OFFSET as i32], 0
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
            Op::CallOut {
                helper,
                ip,
                packed,
                argc,
                lane,
            } => {
                let s = i.fs.expect("a call-out has a frame state");
                self.write_state(s);
                // The lanes ahead of the helper, which leave their result in
                // the window's `dst` register.
                let mut made: Vec<(DynamicLabel, u16)> = Vec::new();
                if let CallLane::Method {
                    obj,
                    arg_base,
                    dst,
                    intr: Some(m),
                } = lane
                {
                    if let Some(l) = self.method_intr(m, obj, arg_base, dst) {
                        made.push((l, dst));
                    }
                }
                if let CallLane::Prop { obj, dst, name } = lane {
                    if let Some(l) = self.prop_ic(obj, dst, name, ip) {
                        made.push((l, dst));
                    }
                }
                // A split `recv.get(k)`'s intrinsic, as Tier C calls it.
                if let (
                    CallLane::Plain {
                        callee,
                        this: Some(this_v),
                        arg_base,
                        dst,
                    },
                    Some(heap),
                    Some(&cl),
                ) = (lane, self.f.heap.as_ref(), self.f.captured.get(&ip))
                {
                    use super::super::inline::{emit_captured_builtin_lane, emit_captured_method_lane, CapturedLane};
                    let l = match cl {
                        CapturedLane::Direct { bits, helper, .. } => {
                            emit_captured_builtin_lane(&mut self.ops, callee, this_v, arg_base, dst, bits, helper, None)
                        }
                        CapturedLane::Proved { op } => {
                            emit_captured_method_lane(&mut self.ops, callee, this_v, arg_base, argc, dst, op, None, heap)
                        }
                    };
                    made.push((l, dst));
                }
                made.extend(self.cross_lanes(lane, ip));
                self.call_written(helper, ip, packed, argc);
                if let Some(&(_, dst)) = made.first() {
                    let join = self.ops.new_dynamic_label();
                    dynasm!(self.ops ; jmp => join);
                    for &(l, _) in &made {
                        dynasm!(self.ops ; => l);
                    }
                    dynasm!(self.ops
                        ; mov rax, [rbx + 8 * dst as i32]
                        ; => join
                    );
                }
                if i.rep != Rep::Void {
                    let (d, st) = self.dst_gpr(v);
                    dynasm!(self.ops ; mov Rq(d), rax);
                    self.done_gpr(d, st);
                }
            }
            Op::Literal {
                plan_ptr,
                shape,
                val_base,
                count,
                dst,
                ip,
            } => {
                let s = i.fs.expect("a literal has a frame state");
                self.write_state(s);
                // SAFETY: a root function's plan, which outlives the VM (the
                // address Tier C bakes for the same literal).
                let plan = unsafe { &*(plan_ptr as *const crate::bytecode::StaticKeyPlan) };
                let ready = super::super::emit_misc::emit_ready_literal_of(&mut self.ops, plan, shape, val_base, count, dst);
                let packed = (u64::from(shape) << 32) | (u64::from(val_base) << 16) | u64::from(count);
                dynasm!(self.ops
                    ; mov rcx, rdi
                    ; mov rdx, rbx
                    ; mov r8, QWORD plan_ptr as i64
                    ; mov r9, QWORD packed as i64
                    ; mov rax, QWORD crate::vm::jit_finalize_object_thin as *const () as i64
                    ; call rax
                );
                self.leave_on_sentinel(ip);
                if let Some(ready) = ready {
                    let join = self.ops.new_dynamic_label();
                    dynasm!(self.ops
                        ; jmp => join
                        ; => ready
                        ; mov rax, [rbx + 8 * dst as i32]
                        ; => join
                    );
                }
                let (d, st) = self.dst_gpr(v);
                dynasm!(self.ops ; mov Rq(d), rax);
                self.done_gpr(d, st);
            }
            Op::IterPure { helper, src } => {
                let stub = stub.expect("a guard has a frame state");
                let rs = self.gpr(src, RDX);
                dynasm!(self.ops
                    ; mov rdx, Rq(rs)
                    ; mov rcx, rdi
                    ; mov rax, QWORD helper as i64
                    ; call rax
                    ; mov r10, QWORD crate::codegen::SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => stub
                );
                let (d, st) = self.dst_gpr(v);
                dynasm!(self.ops ; mov Rq(d), rax);
                self.done_gpr(d, st);
            }
            Op::IterStep {
                ip,
                packed,
                value_dst,
                done_dst,
                iter,
                idx,
            } => {
                // The helper runs the loop's safe point and a user `next`:
                // the state roots every heap value first.
                let s = i.fs.expect("a step has a frame state");
                let heap = f.heap.expect("Tier O bodies have their helpers");
                self.write_state(s);
                let stepped = self.ops.new_dynamic_label();
                if crate::codegen::iter_mirror_step_enabled() {
                    crate::codegen::emit_arr_mirror_iter_step(&mut self.ops, value_dst, done_dst, iter, idx, stepped);
                }
                dynasm!(self.ops
                    ; mov rcx, rdi
                    ; mov rdx, rbx
                    ; mov r8, QWORD packed as i64
                    ; mov r9d, idx as i32
                    ; mov rax, QWORD heap.iter_next as i64
                    ; call rax
                );
                self.leave_on_sentinel(ip);
                dynasm!(self.ops ; => stepped);
            }
            Op::Handler { helper, packed } => {
                dynasm!(self.ops
                    ; mov rcx, rdi
                    ; mov rdx, QWORD packed as i64
                    ; mov rax, QWORD helper as i64
                    ; call rax
                );
            }
            Op::EndFinally { kind, val, ip } => {
                let s = i.fs.expect("a finally's end has a frame state");
                self.write_state(s);
                let (cont, here) = (self.ops.new_dynamic_label(), self.ops.new_dynamic_label());
                let epilogue = self.epilogue;
                dynasm!(self.ops
                    ; mov rcx, rdi
                    ; mov rdx, [rbx + 8 * kind as i32]
                    ; mov r8, [rbx + 8 * val as i32]
                    ; mov rax, QWORD crate::vm::jit_end_finally_route as *const () as i64
                    ; call rax
                    ; cmp rax, -1                         // a normal completion
                    ; je => cont
                    ; mov r10, QWORD crate::codegen::SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => here                          // a return or throw
                );
                // A `break` routed on: the interpreter goes on there.
                self.route_exit();
                dynasm!(self.ops
                    ; => here
                    ; mov DWORD [rsi], ip as i32
                    ; xor eax, eax
                    ; jmp => epilogue
                    ; => cont
                );
            }
            Op::IterCloseFin { kind, ip } => {
                let s = i.fs.expect("an iterator close has a frame state");
                self.write_state(s);
                let cont = self.ops.new_dynamic_label();
                let epilogue = self.epilogue;
                dynasm!(self.ops
                    ; mov rcx, rdi
                    ; mov rdx, [rbx + 8 * kind as i32]
                    ; mov rax, QWORD crate::vm::jit_iter_close_finally as *const () as i64
                    ; call rax
                    ; mov r10, QWORD crate::codegen::SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; jne => cont
                    ; mov DWORD [rsi], ip as i32
                    ; xor eax, eax
                    ; jmp => epilogue
                    ; => cont
                );
            }
            Op::JumpFinally { target, floor } => {
                let s = i.fs.expect("a routed jump has a frame state");
                self.write_state(s);
                dynasm!(self.ops
                    ; mov rcx, rdi
                    ; mov edx, target as i32
                    ; mov r8d, floor as i32
                    ; mov rax, QWORD crate::vm::jit_jump_finally as *const () as i64
                    ; call rax
                );
                self.route_exit();
            }
            Op::Throw { v: x, ip } => {
                // The value pending, the bail a committed throw the
                // interpreter unwinds from.
                let s = i.fs.expect("a throw has a frame state");
                self.write_state(s);
                let rx = self.gpr(x, RDX);
                let epilogue = self.epilogue;
                dynasm!(self.ops
                    ; mov rdx, Rq(rx)
                    ; mov rcx, rdi
                    ; mov rax, QWORD crate::vm::jit_throw as *const () as i64
                    ; call rax
                    ; mov DWORD [rsi], ip as i32
                    ; xor eax, eax
                    ; jmp => epilogue
                );
            }
            Op::ReturnFinally { v: x } => {
                let s = i.fs.expect("a return has a frame state");
                self.write_state(s);
                let rx = self.gpr(x, RDX);
                let cont = self.ops.new_dynamic_label();
                let inter = self.ops.new_dynamic_label();
                dynasm!(self.ops
                    ; mov rdx, Rq(rx)
                    ; mov rcx, rdi
                    ; mov rax, QWORD crate::vm::jit_return_finally as *const () as i64
                    ; call rax
                    ; cmp rax, -2                         // no `finally` to run
                    ; je => cont
                    ; jmp => inter
                    ; => inter
                );
                self.route_exit();
                dynasm!(self.ops ; => cont);
            }
            Op::WindowLoad(r) => {
                let (d, st) = self.dst_gpr(v);
                dynasm!(self.ops ; mov Rq(d), [rbx + 8 * r as i32]);
                self.done_gpr(d, st);
            }
            Op::AllocOut { helper, ip, a, b } => {
                // The collector may run: the state roots every heap value.
                let s = i.fs.expect("an allocation has a frame state");
                self.write_state(s);
                dynasm!(self.ops
                    ; mov rcx, rdi
                    ; mov rdx, rbx
                    ; mov r8, QWORD a as i64
                    ; mov r9, QWORD b as i64
                    ; mov rax, QWORD helper as i64
                    ; call rax
                );
                self.leave_on_sentinel(ip);
                let (d, st) = self.dst_gpr(v);
                dynasm!(self.ops ; mov Rq(d), rax);
                self.done_gpr(d, st);
            }
            Op::NewLane {
                lane,
                callee,
                arg_base,
                dst,
                ip,
                packed,
                argc,
            } => {
                // Tier C's allocation lane over the window the state fills:
                // the instance made in compiled code, else `jit_new_ic`.
                let s = i.fs.expect("a `new` has a frame state");
                self.write_state(s);
                let made = super::super::emit_misc::emit_new_lane(&mut self.ops, lane, callee, arg_base, dst);
                self.call_written(crate::vm::jit_new_ic as *const () as u64, ip, packed, argc);
                let join = self.ops.new_dynamic_label();
                dynasm!(self.ops
                    ; jmp => join
                    ; => made
                    ; mov rax, [rbx + 8 * dst as i32]
                    ; => join
                );
                let (d, st) = self.dst_gpr(v);
                dynasm!(self.ops ; mov Rq(d), rax);
                self.done_gpr(d, st);
            }
            Op::SetPropShape {
                obj,
                val,
                shape,
                slot,
                ip,
                packed,
            } => {
                let s = i.fs.expect("a store has a frame state");
                let ro = self.gpr(obj, RDX);
                let rv = self.gpr(val, R11);
                let miss = self.ops.new_dynamic_label();
                let done = self.ops.new_dynamic_label();
                let hot_off = crate::vm::host_api::JIT_HOT_MIRROR_RAW_OFFSET as i32;
                let hot_vals = crate::vm::host_api::JIT_HOT_VALS_OFF as i32;
                dynasm!(self.ops
                    ; mov rax, Rq(ro)
                    ; shr rax, 48
                    ; cmp eax, 0x7FFD
                    ; jne => miss
                    ; mov ecx, Rd(ro)
                    ; mov r10, [rdi + hot_off]
                    ; lea r10, [r10 + rcx*8]
                    ; cmp DWORD [r10 + rcx*8], shape as i32
                    ; jne => miss
                    ; mov r10, [r10 + rcx*8 + hot_vals]
                    ; mov [r10 + (slot as i32) * 8], Rq(rv)
                    // A primitive needs no barrier.
                    ; mov rax, Rq(rv)
                    ; shr rax, 48
                    ; cmp eax, 0x7FFD
                    ; jne => done
                    ; mov r8, Rq(rv)
                    ; mov edx, ecx
                );
                crate::codegen::emit_heap_store_barrier(&mut self.ops, f.gen_lane, done);
                dynasm!(self.ops ; => miss);
                self.call_out(s, crate::vm::jit_set_prop_slow as *const () as u64, ip, packed, 0);
                dynasm!(self.ops ; => done);
            }
            Op::UpvalGet(idx) => {
                let stub = stub.expect("a guard has a frame state");
                let (d, st) = self.dst_gpr(v);
                let act = crate::vm::host_api::JIT_ACT_UPVALS_OFFSET as i32;
                let mirror = crate::vm::host_api::JIT_CELL_MIRROR_RAW_OFFSET as i32;
                dynasm!(self.ops
                    // The running closure's cells (none: the interpreter's).
                    ; mov rax, [rdi + act]
                    ; test rax, rax
                    ; jz => stub
                    ; mov eax, [rax + (idx as i32) * 4]
                    ; mov rcx, [rdi + mirror]
                    ; mov Rq(d), [rcx + rax * 8]
                    ; mov rax, QWORD Value::UNINITIALIZED.bits() as i64
                    ; cmp Rq(d), rax
                    ; je => stub
                );
                self.done_gpr(d, st);
            }
            Op::StoreGlobal(idx, x) => {
                let stub = stub.expect("a guard has a frame state");
                let rx = self.gpr(x, RDX);
                let base = crate::vm::host_api::JIT_GLOBALS_RAW_OFFSET as i32;
                let off = 8 * idx as i32;
                dynasm!(self.ops
                    // The slot, as Tier C writes it (routes are checked at
                    // entry, and a change since is the interpreter's:
                    // `Vm::jit_route_dirty`); one in its TDZ or never made
                    // is the interpreter's to write.
                    ; cmp BYTE [rdi + crate::vm::host_api::JIT_ROUTE_DIRTY_OFFSET as i32], 0
                    ; jne => stub
                    ; mov rax, [rdi + base]
                    ; mov rcx, QWORD Value::UNINITIALIZED.bits() as i64
                    ; cmp [rax + off], rcx
                    ; je => stub
                    ; mov [rax + off], Rq(rx)
                );
            }
            Op::MathGuard { recv, gidx, op } => {
                let stub = stub.expect("a guard has a frame state");
                let g = f.math.expect("a Math guard has the intrinsic Math");
                let (bits, ver, slot) = (
                    g.op_callee_bits[op as usize],
                    g.op_callee_ver[op as usize],
                    g.op_slot[op as usize],
                );
                match recv {
                    Some(o) => {
                        let ro = self.gpr(o, RDX);
                        dynasm!(self.ops ; mov rax, Rq(ro));
                    }
                    None => {
                        let base = crate::vm::host_api::JIT_GLOBALS_RAW_OFFSET as i32;
                        dynasm!(self.ops
                            // `Math` by its slot, while that is its route.
                            ; cmp BYTE [rdi + crate::vm::host_api::JIT_ROUTE_DIRTY_OFFSET as i32], 0
                            ; jne => stub
                            ; mov rax, [rdi + base]
                            ; mov rax, [rax + 8 * gidx as i32]
                        );
                    }
                }
                let versions = crate::vm::host_api::JIT_VERSIONS_RAW_OFFSET as i32;
                dynasm!(self.ops
                    // The intrinsic `Math`, its shape as it was...
                    ; mov r10, QWORD g.receiver_bits as i64
                    ; cmp rax, r10
                    ; jne => stub
                    ; mov ecx, eax
                    ; mov r11, [rdi + versions]
                    ; cmp DWORD [r11 + rcx * 4], g.receiver_ver as i32
                    ; jne => stub
                    // ...holding the intrinsic method.
                    ; mov rcx, QWORD g.receiver_vals as i64
                    ; mov rax, [rcx + (slot as i32) * 8]
                    ; mov r10, QWORD bits as i64
                    ; cmp rax, r10
                    ; jne => stub
                    ; mov ecx, eax
                    ; cmp DWORD [r11 + rcx * 4], ver as i32
                    ; jne => stub
                );
            }
            Op::GuardCallee { v: x, op } => {
                let stub = stub.expect("a guard has a frame state");
                let g = f.math.expect("a Math guard has the intrinsic Math");
                let rx = self.gpr(x, RDX);
                let versions = crate::vm::host_api::JIT_VERSIONS_RAW_OFFSET as i32;
                dynasm!(self.ops
                    ; mov r10, QWORD g.op_callee_bits[op as usize] as i64
                    ; cmp Rq(rx), r10
                    ; jne => stub
                    ; mov ecx, Rd(rx)
                    ; mov r11, [rdi + versions]
                    ; cmp DWORD [r11 + rcx * 4], g.op_callee_ver[op as usize] as i32
                    ; jne => stub
                );
            }
            Op::MathF1(op, a) => {
                let xa = self.xmm(a, X1);
                let (d, st) = self.dst_xmm(v);
                match op {
                    MathFn::Sqrt => dynasm!(self.ops ; sqrtsd Rx(d), Rx(xa)),
                    MathFn::Abs => dynasm!(self.ops
                        ; movq rax, Rx(xa)
                        ; btr rax, 63
                        ; movq Rx(d), rax
                    ),
                    // roundsd's immediate: bit 3 keeps the precision
                    // exception quiet, bits 0-1 the mode.
                    MathFn::Floor => dynasm!(self.ops ; roundsd Rx(d), Rx(xa), 9),
                    MathFn::Ceil => dynasm!(self.ops ; roundsd Rx(d), Rx(xa), 10),
                    MathFn::Trunc => dynasm!(self.ops ; roundsd Rx(d), Rx(xa), 11),
                    _ => unreachable!("not a one-instruction Math op: {op:?}"),
                }
                self.done_xmm(d, st);
            }
            Op::MathCall1(op, a) => {
                let xa = self.xmm(a, X1);
                dynasm!(self.ops
                    ; movq rdx, Rx(xa)
                    ; mov ecx, op as i32
                    ; mov rax, QWORD crate::vm::jit_math_unary as *const () as i64
                    ; call rax
                );
                let (d, st) = self.dst_xmm(v);
                dynasm!(self.ops ; movq Rx(d), rax);
                self.done_xmm(d, st);
            }
            Op::MathCall2(op, a, b) => {
                let xa = self.xmm(a, X1);
                let xb = self.xmm(b, X2);
                dynasm!(self.ops
                    ; movq rdx, Rx(xa)
                    ; movq r8, Rx(xb)
                    ; mov ecx, op as i32
                    ; mov rax, QWORD crate::vm::jit_math_two as *const () as i64
                    ; call rax
                );
                let (d, st) = self.dst_xmm(v);
                dynasm!(self.ops ; movq Rx(d), rax);
                self.done_xmm(d, st);
            }
            Op::ImulI(a, b) => {
                let ra = self.gpr(a, RDX);
                let rb = self.gpr(b, R11);
                let (d, st) = self.dst_gpr(v);
                dynasm!(self.ops
                    ; mov eax, Rd(ra)
                    ; imul eax, Rd(rb)
                    ; mov Rd(d), eax
                );
                self.done_gpr(d, st);
            }
            Op::ModF(a, b) => {
                let xa = self.xmm(a, X1);
                let xb = self.xmm(b, X2);
                let (d, st) = self.dst_xmm(v);
                let slow = self.ops.new_dynamic_label();
                let conv = self.ops.new_dynamic_label();
                let done = self.ops.new_dynamic_label();
                dynasm!(self.ops
                    // Two integral Numbers, a divisor neither 0 nor -1: the
                    // integer remainder, which has the dividend's sign.
                    ; cvttsd2si rax, Rx(xa)
                    ; cvttsd2si rcx, Rx(xb)
                    ; test rcx, rcx
                    ; jz => slow
                    ; cmp rcx, -1
                    ; je => slow
                    ; cvtsi2sd xmm0, rax
                    ; ucomisd xmm0, Rx(xa)
                    ; jp => slow
                    ; jne => slow
                    ; cvtsi2sd xmm0, rcx
                    ; ucomisd xmm0, Rx(xb)
                    ; jp => slow
                    ; jne => slow
                    ; cqo
                    ; idiv rcx
                    ; test rdx, rdx
                    ; jnz => conv
                    // A zero remainder of a negative dividend is -0.
                    ; movq rax, Rx(xa)
                    ; test rax, rax
                    ; js => slow
                    ; => conv
                    ; cvtsi2sd Rx(d), rdx
                    ; jmp => done
                    ; => slow
                    ; movq rcx, Rx(xa)
                    ; movq rdx, Rx(xb)
                    ; mov rax, QWORD crate::vm::jit_opt_fmod as *const () as i64
                    ; call rax
                    ; movq Rx(d), rax
                    ; => done
                );
                self.done_xmm(d, st);
            }
            Op::ShapeVals { obj, shape } => {
                let stub = stub.expect("a guard has a frame state");
                let ro = self.gpr(obj, RDX);
                let (d, st) = self.dst_gpr(v);
                let hot_off = crate::vm::host_api::JIT_HOT_MIRROR_RAW_OFFSET as i32;
                let hot_vals = crate::vm::host_api::JIT_HOT_VALS_OFF as i32;
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
                    ; mov Rq(d), [r11 + rcx*8 + hot_vals]
                );
                self.done_gpr(d, st);
            }
            Op::LoadSlot { vals, slot } => {
                let rv = self.gpr(vals, RDX);
                let (d, st) = self.dst_gpr(v);
                dynasm!(self.ops ; mov Rq(d), [Rq(rv) + 8 * slot as i32]);
                self.done_gpr(d, st);
            }
            Op::StoreSlot { vals, obj, val, slot } => {
                let rv = self.gpr(vals, RDX);
                let rx = self.gpr(val, R11);
                let done = self.ops.new_dynamic_label();
                dynasm!(self.ops
                    ; mov [Rq(rv) + 8 * slot as i32], Rq(rx)
                    // A primitive needs no barrier.
                    ; mov rax, Rq(rx)
                    ; shr rax, 48
                    ; cmp eax, 0x7FFD
                    ; jne => done
                    ; mov rax, Rq(rx)
                );
                let ro = self.gpr(obj, 1);
                dynasm!(self.ops
                    ; mov edx, Rd(ro)
                    ; mov r8, rax
                );
                crate::codegen::emit_heap_store_barrier(&mut self.ops, f.gen_lane, done);
                dynasm!(self.ops ; => done);
            }
            Op::GetPropShape { obj, shape, slot, slow } => {
                let stub = stub.expect("a guard has a frame state");
                let ro = self.gpr(obj, RDX);
                let (d, st) = self.dst_gpr(v);
                let hot_off = crate::vm::host_api::JIT_HOT_MIRROR_RAW_OFFSET as i32;
                let hot_vals = crate::vm::host_api::JIT_HOT_VALS_OFF as i32;
                let off = 8 * slot as i32;
                let miss = if slow.is_some() { self.ops.new_dynamic_label() } else { stub };
                dynasm!(self.ops
                    ; mov rax, Rq(ro)
                    ; shr rax, 48
                    ; cmp eax, 0x7FFD
                    ; jne => miss
                    ; mov ecx, Rd(ro)
                    ; mov r11, [rdi + hot_off]
                    ; lea r11, [r11 + rcx*8]
                    ; cmp DWORD [r11 + rcx*8], shape as i32
                    ; jne => miss
                    ; mov r11, [r11 + rcx*8 + hot_vals]
                    ; mov Rq(d), [r11 + off]
                );
                if let Some((ip, packed)) = slow {
                    // Another receiver: the whole `GetProp`.
                    let s = i.fs.expect("a read has a frame state");
                    let done = self.ops.new_dynamic_label();
                    dynasm!(self.ops ; jmp => done ; => miss);
                    self.call_out(s, crate::vm::jit_opt_get_prop_slow as *const () as u64, ip, packed, 0);
                    dynasm!(self.ops ; mov Rq(d), rax ; => done);
                }
                self.done_gpr(d, st);
            }
            Op::Length { obj, ip, packed } => {
                let s = i.fs.expect("a read has a frame state");
                let heap = f.heap.expect("Tier O bodies have their helpers");
                let ro = self.gpr(obj, RDX);
                let (d, st) = self.dst_gpr(v);
                let arr_raw = crate::vm::host_api::JIT_ARR_MIRROR_RAW_OFFSET as i32;
                let arr_len = crate::vm::host_api::JIT_ARR_MIRROR_LEN_OFFSET as i32;
                let (slow, got, done) = (
                    self.ops.new_dynamic_label(),
                    self.ops.new_dynamic_label(),
                    self.ops.new_dynamic_label(),
                );
                dynasm!(self.ops
                    // A dense Array's live mirror entry holds its length.
                    ; mov rax, Rq(ro)
                    ; shr rax, 48
                    ; cmp eax, 0x7FFD
                    ; jne => slow
                    ; mov ecx, Rd(ro)
                    ; cmp ecx, [rdi + arr_len]
                    ; jae => slow
                    ; mov r10, [rdi + arr_raw]
                    ; shl rcx, 4
                    ; add r10, rcx
                    ; cmp QWORD [r10], 0
                    ; je => slow
                    ; mov rcx, [r10 + 8]
                    ; cmp rcx, i32::MAX
                    ; ja => slow
                    ; mov rax, QWORD TAG_INT as i64
                    ; or rax, rcx
                    ; mov Rq(d), rax
                    ; jmp => done
                    ; => slow
                );
                // A string's (or another Array's) through the pure helper,
                // then the whole read.
                let ro = self.gpr(obj, RDX);
                dynasm!(self.ops
                    ; mov rdx, Rq(ro)
                    ; mov rcx, rdi
                    ; mov rax, QWORD heap.quick_len as i64
                    ; call rax
                    ; mov r10, QWORD crate::codegen::SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; jne => got
                );
                self.call_out(s, crate::vm::jit_opt_get_prop_slow as *const () as u64, ip, packed, 0);
                dynasm!(self.ops
                    ; => got
                    ; mov Rq(d), rax
                    ; => done
                );
                self.done_gpr(d, st);
            }
            Op::GetPropPoly { obj, ways, ip, packed } => {
                let s = i.fs.expect("a read has a frame state");
                let ro = self.gpr(obj, RDX);
                let (d, st) = self.dst_gpr(v);
                let hot_off = crate::vm::host_api::JIT_HOT_MIRROR_RAW_OFFSET as i32;
                let hot_vals = crate::vm::host_api::JIT_HOT_VALS_OFF as i32;
                let miss = self.ops.new_dynamic_label();
                let done = self.ops.new_dynamic_label();
                let arms: Vec<DynamicLabel> = ways.iter().map(|_| self.ops.new_dynamic_label()).collect();
                dynasm!(self.ops
                    ; mov rax, Rq(ro)
                    ; shr rax, 48
                    ; cmp eax, 0x7FFD
                    ; jne => miss
                    ; mov ecx, Rd(ro)
                    ; mov r11, [rdi + hot_off]
                    ; lea r11, [r11 + rcx*8]
                    ; mov eax, DWORD [r11 + rcx*8]
                );
                for (&(shape, _), &arm) in ways.iter().zip(&arms) {
                    dynasm!(self.ops
                        ; cmp eax, shape as i32
                        ; je => arm
                    );
                }
                dynasm!(self.ops ; jmp => miss);
                for (&(_, slot), &arm) in ways.iter().zip(&arms) {
                    dynasm!(self.ops
                        ; => arm
                        ; mov r10, [r11 + rcx*8 + hot_vals]
                        ; mov Rq(d), [r10 + 8 * slot as i32]
                        ; jmp => done
                    );
                }
                dynasm!(self.ops ; => miss);
                self.call_out(s, crate::vm::jit_opt_get_prop_slow as *const () as u64, ip, packed, 0);
                dynasm!(self.ops
                    ; mov Rq(d), rax
                    ; => done
                );
                self.done_gpr(d, st);
            }
            Op::AddT(a, b) => {
                let s = i.fs.expect("a `+` has a frame state");
                let ip = f.states[s as usize].ip;
                // The collector may run (and coercions): the state roots
                // every heap value held in a register.
                self.write_state(s);
                let ra = self.gpr(a, RDX);
                let rb = self.gpr(b, R11);
                dynasm!(self.ops
                    ; mov rax, Rq(ra)
                    ; mov r8, Rq(rb)
                    ; mov rdx, rax
                    ; mov rcx, rdi
                    ; mov rax, QWORD crate::vm::jit_concat as *const () as i64
                    ; call rax
                );
                self.leave_on_sentinel(ip);
                let (d, st) = self.dst_gpr(v);
                dynasm!(self.ops ; mov Rq(d), rax);
                self.done_gpr(d, st);
            }
            Op::UpvalSet(idx, x) => {
                let s = i.fs.expect("a captured write has a frame state");
                let rx = self.gpr(x, R11);
                let slow = self.ops.new_dynamic_label();
                let done = self.ops.new_dynamic_label();
                let act = crate::vm::host_api::JIT_ACT_UPVALS_OFFSET as i32;
                let mirror = crate::vm::host_api::JIT_CELL_MIRROR_RAW_OFFSET as i32;
                let const_ne = crate::vm::host_api::JIT_CONST_CELLS_NE_OFFSET as i32;
                let fn_name_ne = crate::vm::host_api::JIT_FN_NAME_CELLS_NE_OFFSET as i32;
                dynasm!(self.ops
                    // A primitive into a cell that is neither const nor in
                    // its TDZ, through the cell-value mirror (Tier C's lane);
                    // anything else through the helper, which owns the
                    // barrier. Only scratch registers until the helper: the
                    // state it writes reads every other.
                    ; mov rax, [rdi + act]
                    ; test rax, rax
                    ; jz => slow
                    ; mov ecx, [rax + (idx as i32) * 4]
                    ; cmp BYTE [rdi + const_ne], 0
                    ; jne => slow
                    ; cmp BYTE [rdi + fn_name_ne], 0
                    ; jne => slow
                    ; mov rax, Rq(rx)
                    ; mov rdx, rax
                    ; shr rdx, 48
                    ; cmp edx, 0x7FFD
                    ; je => slow
                    ; mov r10, [rdi + mirror]
                    ; mov rdx, QWORD Value::UNINITIALIZED.bits() as i64
                    ; cmp [r10 + rcx * 8], rdx
                    ; je => slow
                    ; mov [r10 + rcx * 8], rax
                    ; jmp => done
                    ; => slow
                );
                self.write_state(s);
                let rx = self.gpr(x, R11);
                dynasm!(self.ops
                    ; mov r8, Rq(rx)
                    ; mov edx, idx as i32
                    ; mov rcx, rdi
                    ; mov rax, QWORD crate::vm::jit_tierc_upval_set as *const () as i64
                    ; call rax
                    ; mov r10, QWORD crate::codegen::SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; jne => done
                );
                // A const or TDZ cell: the interpreter throws.
                self.leave_counted(s);
                dynasm!(self.ops ; => done);
            }
            Op::GetIndex {
                obj,
                key,
                ip,
                packed,
                ta,
            } => {
                let s = i.fs.expect("a read has a frame state");
                let (d, st) = self.dst_gpr(v);
                let slow = self.ops.new_dynamic_label();
                let full = self.ops.new_dynamic_label();
                let got = self.ops.new_dynamic_label();
                let done = self.ops.new_dynamic_label();
                let ta_lane = ta.map(|k| (self.ta_base + TA_RECORD * i32::from(k), self.ops.new_dynamic_label()));
                self.index_to_rax(key, slow);
                let ro = self.gpr(obj, RDX);
                self.mirror_entry(ro, ta_lane.map_or(slow, |(_, l)| l));
                dynasm!(self.ops
                    ; mov r10, [r10 + rax * 8]
                    ; mov rcx, QWORD crate::codegen::ARR_HOLE_BITS as i64
                    ; cmp r10, rcx
                    ; je => slow
                    ; mov Rq(d), r10
                    ; jmp => done
                );
                if let Some((rec, lane)) = ta_lane {
                    // The typed array the register held before: its element
                    // through Tier C's stub.
                    let fill = self.ops.new_dynamic_label();
                    dynasm!(self.ops ; => lane);
                    self.ta_record_check(ro, rec, fill, slow);
                    let stub = self.ta_read;
                    dynasm!(self.ops
                        ; cmp rax, r11
                        ; jae => slow
                        ; mov rdx, rax
                        ; mov r9, [rsp + rec + 8]
                        ; movzx ecx, BYTE [rsp + rec + 24]
                        ; call => stub
                        ; jmp => got
                        ; => fill
                    );
                    let ro = self.gpr(obj, RDX);
                    dynasm!(self.ops ; mov rdx, Rq(ro));
                    self.key_to_r8(key);
                    dynasm!(self.ops
                        ; lea r9, [rsp + rec]
                        ; mov rcx, rdi
                        ; mov rax, QWORD crate::vm::jit_get_index_ta_fill as *const () as i64
                        ; call rax
                        ; mov r10, QWORD crate::codegen::SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; jne => got
                        ; jmp => full
                    );
                }
                // The pure helper (strings, holes, string keys), then the
                // whole op.
                dynasm!(self.ops ; => slow);
                let ro = self.gpr(obj, RDX);
                dynasm!(self.ops ; mov rdx, Rq(ro));
                self.key_to_r8(key);
                dynasm!(self.ops
                    ; mov rcx, rdi
                    ; mov rax, QWORD crate::vm::jit_get_index as *const () as i64
                    ; call rax
                    ; mov r10, QWORD crate::codegen::SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; jne => got
                    ; => full
                );
                self.call_out(s, crate::vm::jit_opt_index_full as *const () as u64, ip, packed, 0);
                dynasm!(self.ops
                    ; => got
                    ; mov Rq(d), rax
                    ; => done
                );
                self.done_gpr(d, st);
            }
            Op::SetIndex {
                obj,
                key,
                val,
                ip,
                packed,
                ta,
            } => {
                let s = i.fs.expect("a write has a frame state");
                let slow = self.ops.new_dynamic_label();
                let full = self.ops.new_dynamic_label();
                let done = self.ops.new_dynamic_label();
                let ta_lane = ta.map(|k| (self.ta_base + TA_RECORD * i32::from(k), self.ops.new_dynamic_label()));
                self.index_to_rax(key, slow);
                let ro = self.gpr(obj, RDX);
                self.mirror_entry(ro, ta_lane.map_or(slow, |(_, l)| l));
                dynasm!(self.ops
                    ; lea r10, [r10 + rax * 8]
                    ; mov rcx, QWORD crate::codegen::ARR_HOLE_BITS as i64
                    ; cmp [r10], rcx
                    ; je => slow                          // a hole: not an overwrite
                );
                let rv = self.gpr(val, 1);
                dynasm!(self.ops
                    ; mov [r10], Rq(rv)
                    ; mov rax, Rq(rv)
                    ; shr rax, 48
                    ; cmp eax, 0x7FFD
                    ; jne => done                         // a primitive: no barrier
                    ; mov edx, Rd(ro)
                    ; mov r8, Rq(rv)
                );
                crate::codegen::emit_heap_store_barrier(&mut self.ops, f.gen_lane, done);
                if let Some((rec, lane)) = ta_lane {
                    // The typed array the register held before, writable: a
                    // Number its kind stores exactly, through Tier C's stub.
                    let fill = self.ops.new_dynamic_label();
                    dynasm!(self.ops ; => lane);
                    let ro = self.gpr(obj, RDX);
                    self.ta_record_check(ro, rec, fill, slow);
                    let stub = self.ta_write;
                    dynasm!(self.ops
                        ; cmp BYTE [rsp + rec + 25], 0
                        ; je => slow
                        ; cmp rax, r11
                        ; jae => slow
                        // The stub's r8 and r9 may hold this op's operands,
                        // which a value the stub declines (a double into an
                        // integer kind) takes to the slow path.
                        ; mov [rsp + 32], r8
                        ; mov [rsp + 40], r9
                    );
                    let rv = self.gpr(val, 8);
                    dynasm!(self.ops
                        ; mov r8, Rq(rv)
                        ; mov rdx, rax
                        ; mov r9, [rsp + rec + 8]
                        ; movzx ecx, BYTE [rsp + rec + 24]
                        ; call => stub
                        ; mov r8, [rsp + 32]
                        ; mov r9, [rsp + 40]
                        ; test eax, eax
                        ; jnz => done
                        ; jmp => slow
                        ; => fill
                    );
                    let rv = self.gpr(val, RAX);
                    dynasm!(self.ops ; mov [rsp + 40], Rq(rv));
                    let ro = self.gpr(obj, RDX);
                    dynasm!(self.ops ; mov rdx, Rq(ro));
                    self.key_to_r8(key);
                    dynasm!(self.ops
                        ; mov r9, [rsp + 40]
                        ; lea r10, [rsp + rec]
                        ; mov [rsp + 32], r10
                        ; mov rcx, rdi
                        ; mov rax, QWORD crate::vm::jit_set_index_ta_fill as *const () as i64
                        ; call rax
                        ; mov r10, QWORD crate::codegen::SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; jne => done
                        ; jmp => full
                    );
                }
                dynasm!(self.ops ; => slow);
                // The pure helper (growth, holes), then the whole op.
                let rv = self.gpr(val, RAX);
                dynasm!(self.ops ; mov [rsp + 40], Rq(rv));
                let ro = self.gpr(obj, RDX);
                dynasm!(self.ops ; mov rdx, Rq(ro));
                self.key_to_r8(key);
                dynasm!(self.ops
                    ; mov r9, [rsp + 40]
                    ; mov rcx, rdi
                    ; mov rax, QWORD crate::vm::jit_set_index as *const () as i64
                    ; call rax
                    ; mov r10, QWORD crate::codegen::SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; jne => done
                    ; => full
                );
                self.call_out(s, crate::vm::jit_opt_index_full as *const () as u64, ip, packed, 0);
                dynasm!(self.ops ; => done);
            }
            Op::CmpT(c, x, y) => {
                let stub = stub.expect("a guard has a frame state");
                let ra = self.gpr(x, RDX);
                let rb = self.gpr(y, R11);
                let (d, st) = self.dst_gpr(v);
                let l: Vec<DynamicLabel> = (0..6).map(|_| self.ops.new_dynamic_label()).collect();
                let (a_int, a_ok, b_int, b_ok, strs, done) = (l[0], l[1], l[2], l[3], l[4], l[5]);
                let (a_null, a_ok_nan, b_null, b_ok_nan) = (
                    self.ops.new_dynamic_label(),
                    self.ops.new_dynamic_label(),
                    self.ops.new_dynamic_label(),
                    self.ops.new_dynamic_label(),
                );
                dynasm!(self.ops
                    ; mov rax, Rq(ra)
                    ; mov rcx, Rq(rb)
                    // Each operand's ToNumber into xmm0 / xmm1 when it runs
                    // no code: a Number, a boolean, `null` (0), `undefined`
                    // (NaN); a heap value may be a string.
                    ; mov rdx, rax
                    ; shr rdx, 48
                    ; cmp edx, 0x7FF9
                    ; je => a_int
                    ; cmp edx, 0x7FFA
                    ; je => a_int                         // a boolean's 0 or 1
                    ; cmp edx, 0x7FFC
                    ; je => a_ok_nan
                    ; cmp edx, 0x7FFB
                    ; je => a_null
                    ; sub edx, 0x7FF9
                    ; cmp edx, 4
                    ; jbe => strs
                    ; movq xmm0, rax
                    ; jmp => a_ok
                    ; => a_null
                    ; mov rdx, QWORD TAG_NULL as i64
                    ; cmp rax, rdx
                    ; jne => strs                         // a small BigInt
                    ; xorps xmm0, xmm0
                    ; jmp => a_ok
                    ; => a_ok_nan
                    ; mov rdx, QWORD QNAN as i64
                    ; movq xmm0, rdx
                    ; jmp => a_ok
                    ; => a_int
                    ; cvtsi2sd xmm0, eax
                    ; => a_ok
                    ; mov rdx, rcx
                    ; shr rdx, 48
                    ; cmp edx, 0x7FF9
                    ; je => b_int
                    ; cmp edx, 0x7FFA
                    ; je => b_int
                    ; cmp edx, 0x7FFC
                    ; je => b_ok_nan
                    ; cmp edx, 0x7FFB
                    ; je => b_null
                    ; sub edx, 0x7FF9
                    ; cmp edx, 4
                    ; jbe => strs
                    ; movq xmm1, rcx
                    ; jmp => b_ok
                    ; => b_null
                    ; mov rdx, QWORD TAG_NULL as i64
                    ; cmp rcx, rdx
                    ; jne => strs
                    ; xorps xmm1, xmm1
                    ; jmp => b_ok
                    ; => b_ok_nan
                    ; mov rdx, QWORD QNAN as i64
                    ; movq xmm1, rdx
                    ; jmp => b_ok
                    ; => b_int
                    ; cvtsi2sd xmm1, ecx
                    ; => b_ok
                );
                match c {
                    Cmp::Lt => dynasm!(self.ops ; ucomisd xmm1, xmm0 ; seta al),
                    Cmp::Le => dynasm!(self.ops ; ucomisd xmm1, xmm0 ; setae al),
                    Cmp::Gt => dynasm!(self.ops ; ucomisd xmm0, xmm1 ; seta al),
                    Cmp::Ge => dynasm!(self.ops ; ucomisd xmm0, xmm1 ; setae al),
                    Cmp::Eq | Cmp::Ne => unreachable!("a relational compare"),
                }
                let op = match c {
                    Cmp::Lt => 0,
                    Cmp::Le => 1,
                    Cmp::Gt => 2,
                    _ => 3,
                };
                dynasm!(self.ops
                    ; movzx eax, al
                    ; mov Rd(d), eax
                    ; jmp => done
                    ; => strs
                    ; mov r8, rcx
                    ; mov rdx, rax
                    ; mov rcx, rdi
                    ; mov r9d, op
                    ; mov rax, QWORD crate::vm::jit_str_relational as *const () as i64
                    ; call rax
                    ; cmp eax, 2
                    ; je => stub
                    ; mov Rd(d), eax
                    ; => done
                );
                self.done_gpr(d, st);
            }
            Op::ClassConst { bits, epoch } => {
                let stub = stub.expect("a guard has a frame state");
                let (d, st) = self.dst_gpr(v);
                dynasm!(self.ops
                    ; cmp DWORD [rdi + crate::vm::host_api::JIT_MI_CLASS_EPOCH_OFFSET as i32], epoch as i32
                    ; jne => stub
                    ; mov Rq(d), QWORD bits as i64
                );
                self.done_gpr(d, st);
            }
            Op::StrictEqH(eq, x, y) => {
                let ra = self.gpr(x, RDX);
                let rb = self.gpr(y, R11);
                let (d, st) = self.dst_gpr(v);
                let l: Vec<DynamicLabel> = (0..9).map(|_| self.ops.new_dynamic_label()).collect();
                let (differ, mixed, a_int, a_done, b_int, b_done, is_true, is_false, heap2) =
                    (l[0], l[1], l[2], l[3], l[4], l[5], l[6], l[7], l[8]);
                let done = self.ops.new_dynamic_label();
                dynasm!(self.ops
                    ; mov rax, Rq(ra)
                    ; mov rcx, Rq(rb)
                    ; cmp rax, rcx
                    ; jne => differ
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
                    ; je => heap2
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
                    ; jmp => is_true
                    // Two heap values: strings compare by content.
                    ; => heap2
                    ; mov r8, rcx
                    ; mov rdx, rax
                    ; mov rcx, rdi
                    ; mov rax, QWORD crate::vm::jit_strict_eq as *const () as i64
                    ; call rax
                    ; test eax, eax
                    ; jz => is_false
                    ; => is_true
                    ; mov Rd(d), i32::from(eq)
                    ; jmp => done
                    ; => is_false
                    ; mov Rd(d), i32::from(!eq)
                    ; => done
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
    let ta_base = OUT_AREA + xmm_area + 8 * a.slots as i32;
    let c3 = ta_base + TA_RECORD * i32::from(f.ta_records);
    let mut frame = c3 + if f.cross.is_empty() && f.ic_sites.is_empty() { 0 } else { C3_SCRATCH };
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
    // No typed array recorded yet: no receiver's bits are 0.
    for k in 0..i32::from(f.ta_records) {
        dynasm!(ops ; mov QWORD [rsp + ta_base + TA_RECORD * k], 0);
    }
    let (ta_read, ta_write) = (ops.new_dynamic_label(), ops.new_dynamic_label());
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
            Term::Jump(_) | Term::Exit => {}
        }
    }
    let fused = |b: &super::build::Block| -> Option<V> {
        let Term::Branch { cond, .. } = b.term else {
            return None;
        };
        (uses[cond as usize] == 1
            && b.insts.last() == Some(&cond)
            && matches!(f.insts[cond as usize].op, Op::CmpF(..) | Op::CmpI(..)))
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
        ta_base,
        ta_read,
        ta_write,
        c3,
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
            let lf = em.ops.new_dynamic_label();
            let lt = em.ops.new_dynamic_label();
            match f.insts[c as usize].op {
                Op::CmpF(cmp, x, y) => {
                    let xa = em.xmm(x, X1);
                    let xb = em.xmm(y, X2);
                    match cmp {
                        Cmp::Lt => dynasm!(em.ops ; ucomisd Rx(xb), Rx(xa) ; jbe => lf),
                        Cmp::Le => dynasm!(em.ops ; ucomisd Rx(xb), Rx(xa) ; jb => lf),
                        Cmp::Gt => dynasm!(em.ops ; ucomisd Rx(xa), Rx(xb) ; jbe => lf),
                        Cmp::Ge => dynasm!(em.ops ; ucomisd Rx(xa), Rx(xb) ; jb => lf),
                        Cmp::Eq => dynasm!(em.ops ; ucomisd Rx(xa), Rx(xb) ; jne => lf ; jp => lf),
                        Cmp::Ne => dynasm!(em.ops ; ucomisd Rx(xa), Rx(xb) ; jp => lt ; je => lf),
                    }
                }
                Op::CmpI(cmp, x, y) => {
                    let ra = em.gpr(x, RDX);
                    let rb = em.gpr(y, R11);
                    dynasm!(em.ops ; cmp Rd(ra), Rd(rb));
                    match cmp {
                        Cmp::Lt => dynasm!(em.ops ; jge => lf),
                        Cmp::Le => dynasm!(em.ops ; jg => lf),
                        Cmp::Gt => dynasm!(em.ops ; jle => lf),
                        Cmp::Ge => dynasm!(em.ops ; jl => lf),
                        Cmp::Eq => dynasm!(em.ops ; jne => lf),
                        Cmp::Ne => dynasm!(em.ops ; je => lf),
                    }
                }
                _ => unreachable!("only a compare fuses"),
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
            // The block's last op has left the body.
            Term::Exit => {}
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
            ; mov r8d, s.why as i32
            ; mov r9d, f.body_gen as i32
            ; mov rax, QWORD helper
            ; call rax
            ; xor eax, eax
            ; jmp => epilogue
        );
    }
    // ── typed-array element stubs ──
    if f.ta_records > 0 {
        super::super::proto_mem::emit_ta_read_stub(&mut em.ops, ta_read);
        super::super::proto_mem::emit_ta_write_stub(&mut em.ops, ta_write);
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
    if let Some(dir) = crate::env_var_os("ZIPP_OPT_DUMP") {
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
