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

type Asm = crate::codegen::X64Asm;

struct Em<'a> {
    ops: Asm,
    f: &'a Func,
    a: &'a Alloc,
    prop_key_ids: &'a [u32],
    slot_base: i32,
    stubs: Vec<DynamicLabel>,
    stub_used: Vec<bool>,
    epilogue: DynamicLabel,
    stress: bool,
    /// The frame offset of the first typed-array record (`Func::ta_records`,
    /// 40 bytes each).
    ta_base: i32,
    /// The 64-byte scratch of Tier C's cross-call lanes (`emit_cross3_invoke`).
    c3: i32,
    /// VM-wide shared JIT helper stub addresses (compiled once per `Jit`).
    shared: &'a super::super::proto_mem::SharedStubAddrs,
    /// Deferred non-deopt bail stubs: `(label, fs_id, ip)`.
    plain_bails: Vec<(DynamicLabel, u32, u32)>,
    /// Deferred raw bail stubs (frame state already in window), deduplicated by `ip`.
    raw_bails: Vec<(DynamicLabel, u32)>,
    /// Deferred switch jump tables `(table_label, slot_labels)`.
    pending_jtabs: Vec<(DynamicLabel, Vec<DynamicLabel>)>,
    /// Out-of-line cold paths emitted after all hot basic blocks.
    deferred_cold: Vec<ColdTask>,
    /// Whether `prop_ic`/`set_prop_ic` must spill/restore `r13`/`r14` around probes.
    save_ic_r13: bool,
    save_ic_r14: bool,
    /// Per-block cache of `(bytecode_reg, ssa_value)` already boxed into
    /// `[rbx + 8*r]` by an earlier unconditional `write_state` in this block.
    window_known: Vec<(u16, V)>,
    /// Fixed 48-byte frame area for cold paths to save/restore volatile
    /// allocator registers (`r8`, `r9`, `xmm4`, `xmm5`) without shifting `rsp`.
    vol_save: i32,
    uses_r8: bool,
    uses_r9: bool,
    uses_x4: bool,
    uses_x5: bool,
}

enum ColdTask {
    StoreBarrierNoVol {
        lbl: DynamicLabel,
        done: DynamicLabel,
        obj: Option<V>,
    },
    CharCodeAtSlow {
        lbl: DynamicLabel,
        done: DynamicLabel,
        obj: V,
        idx: V,
        d: u8,
        stub: DynamicLabel,
    },
    StrictEqAtom {
        lbl: DynamicLabel,
        a: V,
        eq: bool,
        dst: Option<(u8, DynamicLabel)>,
        branch: Option<(DynamicLabel, DynamicLabel)>,
    },
    TaggedTruthy {
        lbl: DynamicLabel,
        a: V,
        truthy: DynamicLabel,
        falsy: DynamicLabel,
    },
    TypeOfIs {
        lbl: DynamicLabel,
        code_neg: i32,
        d: u8,
        done: DynamicLabel,
    },
    GetPropSlow {
        lbl: DynamicLabel,
        done: DynamicLabel,
        obj: V,
        d: u8,
        s: u32,
        ip: u32,
        packed: u64,
    },
    SetPropSlow {
        lbl: DynamicLabel,
        done: DynamicLabel,
        obj: V,
        val: V,
        s: u32,
        ip: u32,
        packed: u64,
    },
    CallOutPropMiss {
        lbl: DynamicLabel,
        join: DynamicLabel,
        obj: u16,
        name: u32,
        s: u32,
        ip: u32,
        packed: u64,
        helper: u64,
        argc: u16,
        ic_miss: Option<(u32, bool)>,
    },
    CallOutSetPropMiss {
        lbl: DynamicLabel,
        done: DynamicLabel,
        val: u16,
        name: u32,
        s: u32,
        ip: u32,
        packed: u64,
        helper: u64,
        argc: u16,
        ic_site: u32,
        ic_direct: bool,
    },
    CallOutFallback {
        lbl: DynamicLabel,
        join: DynamicLabel,
        write_s: Option<u32>,
        helper: u64,
        ip: u32,
        packed: u64,
        argc: u16,
    },
    LengthSlow {
        lbl: DynamicLabel,
        done: DynamicLabel,
        obj: V,
        d: u8,
        s: u32,
        ip: u32,
        packed: u64,
    },
    GetIndexSlow {
        lbl: DynamicLabel,
        done: DynamicLabel,
        ta_fill: Option<(DynamicLabel, i32)>,
        obj: V,
        key: V,
        d: u8,
        s: u32,
        ip: u32,
        packed: u64,
        inlined: bool,
    },
    SetIndexSlow {
        lbl: DynamicLabel,
        done: DynamicLabel,
        ta_fill: Option<(DynamicLabel, i32)>,
        obj: V,
        key: V,
        val: V,
        s: u32,
        ip: u32,
        packed: u64,
    },
    AddTSlow {
        lbl: DynamicLabel,
        done: DynamicLabel,
        a: V,
        b: V,
        dst: Option<u8>,
        s: u32,
        ip: u32,
    },
    UpvalSetSlow {
        lbl: DynamicLabel,
        done: DynamicLabel,
        idx: u16,
        x: V,
        s: u32,
    },
    ModFSlow {
        lbl: DynamicLabel,
        done: DynamicLabel,
        a: V,
        b: V,
        d: u8,
    },
    CmpTSlow {
        lbl: DynamicLabel,
        done: DynamicLabel,
        op: i32,
        d: u8,
        stub: DynamicLabel,
    },
    InstanceOfSlow {
        lbl: DynamicLabel,
        done: DynamicLabel,
        d: u8,
        s: u32,
        ip: u32,
        packed: u64,
    },
    MakeHelperSlow {
        lbl: DynamicLabel,
        done: DynamicLabel,
        helper: u64,
        s: u32,
        ip: u32,
    },
    LiteralSlow {
        lbl: DynamicLabel,
        join: DynamicLabel,
        plan_ptr: u64,
        packed: u64,
        ip: u32,
    },
    NewLaneSlow {
        lbl: DynamicLabel,
        join: DynamicLabel,
        ip: u32,
        packed: u64,
        argc: u16,
    },
    IterStepSlow {
        lbl: DynamicLabel,
        done: DynamicLabel,
        packed: u64,
        idx: u16,
        ip: u32,
    },
    ThisCheckSlow {
        lbl: DynamicLabel,
        done: DynamicLabel,
        d: u8,
        bail: DynamicLabel,
    },
    SuperCtorFetchSlow {
        lbl: DynamicLabel,
        done: DynamicLabel,
        d: u8,
        home_class_id: u32,
    },
    ColdOpSlow {
        lbl: DynamicLabel,
        done: DynamicLabel,
        d: u8,
        s: u32,
        ip: u32,
        cold_op: u16,
    },
}

impl Em<'_> {
    fn invalidate_window_reg(&mut self, reg: u16) {
        self.window_known.retain(|&(r, _)| r != reg);
    }

    fn invalidate_window_state(&mut self, s: u32) {
        let f = self.f;
        for &(r, v) in &f.states[s as usize].regs {
            if !self.window_known.contains(&(r, v)) {
                self.invalidate_window_reg(r);
            }
        }
    }

    fn add_plain_bail(&mut self, s: u32, ip: u32) -> DynamicLabel {
        let lbl = self.ops.new_dynamic_label();
        self.plain_bails.push((lbl, s, ip));
        lbl
    }

    fn add_raw_bail(&mut self, ip: u32) -> DynamicLabel {
        if let Some(&(lbl, _)) = self.raw_bails.iter().find(|&&(_, i)| i == ip) {
            return lbl;
        }
        let lbl = self.ops.new_dynamic_label();
        self.raw_bails.push((lbl, ip));
        lbl
    }

    fn use_stub(&mut self, fs: Option<u32>) -> DynamicLabel {
        let s = fs.expect("a guard has a frame state");
        self.stub_used[s as usize] = true;
        self.stubs[s as usize]
    }

    fn reg_hi(&self) -> i32 {
        ((self.f.reg_count.max(1) as u32) << 16) as i32
    }

    fn prop_key_id(&self, name: u32) -> u32 {
        self.prop_key_ids.get(name as usize).copied().unwrap_or(0)
    }

    fn mega_prop_addr(&self, direct: bool) -> i64 {
        if direct {
            self.shared.mega_prop_full as i64
        } else {
            self.shared.mega_prop_dict as i64
        }
    }

    fn mega_set_addr(&self, direct: bool) -> i64 {
        if direct {
            self.shared.mega_set_full as i64
        } else {
            self.shared.mega_set_dict as i64
        }
    }

    fn can_cross3_dyn(&self) -> bool {
        crate::codegen::cross3_enabled()
            && crate::codegen::cross3_dyn_enabled()
            && crate::codegen::cross3_inline_enabled()
            && crate::codegen::cross3_grow_enabled()
            && crate::codegen::cross3_sloppy_enabled()
    }
}

/// The cross-call lanes' scratch bytes.
const C3_SCRATCH: i32 = 64;

/// Volatile allocator register save area (`r8`, `r9`, `xmm4`, `xmm5`): 48 bytes.
const VOL_SAVE_AREA: i32 = 48;

/// A typed-array record's bytes (`jit_get_index_ta_fill`).
const TA_RECORD: i32 = 40;

impl Em<'_> {
    fn slot(&self, s: u32) -> i32 {
        self.slot_base + 8 * s as i32
    }

    fn rep(&self, v: V) -> Rep {
        self.f.insts[v as usize].rep
    }

    /// `v`'s GPR, loading a spilled one, reading a read-only parameter, or
    /// materializing an immediate into `scratch`.
    fn gpr(&mut self, v: V, scratch: u8) -> u8 {
        match self.a.loc[v as usize] {
            Loc::Gpr(r) => r,
            Loc::Slot(s) => {
                let off = self.slot(s);
                dynasm!(self.ops ; mov Rq(scratch), [rsp + off]);
                scratch
            }
            Loc::Param(r) => {
                let off = 8 * r as i32;
                dynasm!(self.ops ; mov Rq(scratch), [rbx + off]);
                scratch
            }
            Loc::Imm => {
                match self.f.insts[v as usize].op {
                    Op::ConstI32(c) => dynasm!(self.ops ; mov Rd(scratch), c),
                    Op::ConstBool(b) => dynasm!(self.ops ; mov Rd(scratch), i32::from(b)),
                    Op::ConstTagged(bits) => dynasm!(self.ops ; mov Rq(scratch), QWORD bits as i64),
                    _ => unreachable!("non-constant op allocated to Loc::Imm"),
                }
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
            Loc::Gpr(_) | Loc::Param(_) | Loc::Imm => unreachable!("value {v} is not in the XMM class"),
        }
    }

    /// Where to make `v` (a GPR value), and the slot to store it to after.
    fn dst_gpr(&self, v: V) -> (u8, Option<i32>) {
        match self.a.loc[v as usize] {
            Loc::Gpr(r) => (r, None),
            Loc::Slot(s) => (R10, Some(self.slot(s))),
            Loc::Xmm(_) | Loc::Param(_) | Loc::Imm => unreachable!(),
        }
    }

    fn dst_xmm(&self, v: V) -> (u8, Option<i32>) {
        match self.a.loc[v as usize] {
            Loc::Xmm(r) => (r, None),
            Loc::Slot(s) => (X3, Some(self.slot(s))),
            Loc::Gpr(_) | Loc::Param(_) | Loc::Imm => unreachable!(),
        }
    }

    /// Save all volatile allocator registers actually used by this function
    /// into `[rsp + self.vol_save]`. Preserves `rax`, `rcx`, `rdx`, `r10`,
    /// `r11`, and CPU flags without shifting `rsp`.
    fn save_vol(&mut self) {
        let vs = self.vol_save;
        if self.uses_r8 {
            dynasm!(self.ops ; mov [rsp + vs], r8);
        }
        if self.uses_r9 {
            dynasm!(self.ops ; mov [rsp + vs + 8], r9);
        }
        if self.uses_x4 {
            dynasm!(self.ops ; movdqu [rsp + vs + 16], xmm4);
        }
        if self.uses_x5 {
            dynasm!(self.ops ; movdqu [rsp + vs + 32], xmm5);
        }
    }

    /// Restore all volatile allocator registers saved by `save_vol()`.
    /// Preserves `rax`, `rcx`, `rdx`, `r10`, `r11`, and CPU flags.
    fn restore_vol(&mut self) {
        let vs = self.vol_save;
        if self.uses_r8 {
            dynasm!(self.ops ; mov r8, [rsp + vs]);
        }
        if self.uses_r9 {
            dynasm!(self.ops ; mov r9, [rsp + vs + 8]);
        }
        if self.uses_x4 {
            dynasm!(self.ops ; movdqu xmm4, [rsp + vs + 16]);
        }
        if self.uses_x5 {
            dynasm!(self.ops ; movdqu xmm5, [rsp + vs + 32]);
        }
    }

    /// Nursery store barrier that preserves volatile allocator registers
    /// (`r8`, `r9`, `xmm4`, `xmm5`), allowing `StoreSlot`, `SetPropShape`,
    /// `SetIndex`, and unchecked `CellSet` to stay out of `Op::calls()`.
    /// Input: `edx` = holder heap index, `rax` = heap value bits.
    /// Clobbers: `rax`, `rcx`, `rdx`, `r10`, `r11`.
    fn emit_store_barrier_no_vol(&mut self, done: DynamicLabel) {
        if self.f.gen_lane {
            dynasm!(self.ops
                ; mov r11, [rdi + crate::vm::host_api::JIT_GEN_RAW_OFFSET as i32]
                ; test BYTE [r11 + rdx], crate::heap::JIT_GEN_STATE_MASK as i8
                ; jz => done
            );
            if crate::codegen::barrier_filter_enabled() {
                dynasm!(self.ops
                    ; mov r10d, eax
                    ; test BYTE [r11 + r10], crate::heap::JIT_GEN_STATE_MASK as i8
                    ; jnz => done
                );
            }
        }
        self.save_vol();
        dynasm!(self.ops
            ; mov r8, rax
            ; mov rcx, rdi
            ; mov rax, QWORD crate::vm::jit_shape_set_barrier as *const () as i64
            ; call rax
        );
        self.restore_vol();
        dynasm!(self.ops ; jmp => done);
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
        self.write_state_cond(s);
        self.call_written(helper, ip, packed, argc);
    }

    /// Tier C's emitted cross-call lanes for the call at `ip` (`CallLane`,
    /// `Func::cross`), the frame state already in the window they read:
    /// a lane that makes the call leaves its result in the window's `dst`
    /// register and jumps to the returned label; every miss falls through to
    /// the call-out's helper after them. A throw leaves the body at `ip`.
    fn cross_lanes(&mut self, lane: CallLane, ip: u32, packed: u64, argc: u16) -> Option<(DynamicLabel, u16)> {
        use super::super::{emit_cross3_call, emit_cross3_method_call, Cross3Callee};
        let f = self.f;
        let heap = f.heap.as_ref()?;
        let site = f.cross.get(&ip).copied();
        let dyn_ok = self.can_cross3_dyn() && argc <= 255;
        let arms = site.is_some_and(|s| !s.cross3.is_empty() && (!dyn_ok || s.cross3.len() <= 1));
        let own = site
            .and_then(|s| s.cross3m)
            .filter(|_| super::super::tierc_cross3m_enabled());
        let class_guard = f.class_read.get(&ip).copied();
        let proto_guard = site.and_then(|s| s.proto_read);
        let class_method = site.is_some_and(|s| s.class_method);
        let has_class_lane = class_method
            && (arms || (dyn_ok && (class_guard.is_some() || proto_guard.is_some())));
        let mega_key_id = match lane {
            CallLane::Method { mega_ok: true, .. }
                if dyn_ok && !crate::vm::icstats_enabled() =>
            {
                self.prop_key_id((packed >> 32) as u32)
            }
            _ => 0,
        };
        let has_method_lane = has_class_lane || mega_key_id != 0;
        let dst = match lane {
            CallLane::None => return None,
            CallLane::Plain { dst, .. } if arms || dyn_ok => dst,
            CallLane::Method { dst, .. } if own.is_some() || has_method_lane => dst,
            _ => return None,
        };
        let cross_done = self.ops.new_dynamic_label();
        let bail = self.add_raw_bail(ip);
        let (regs, c3) = (f.reg_count, self.c3);
        match lane {
            CallLane::Plain {
                callee,
                this,
                arg_base,
                dst,
            } => {
                if arms {
                    if let Some(site) = site {
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
                }
                if dyn_ok {
                    let stub = self.shared.cross3_dyn as i64;
                    let dyn_miss = self.ops.new_dynamic_label();
                    let af = if callee < regs { 0x200 } else { 0 };
                    let reg_hi = self.reg_hi();
                    dynasm!(self.ops
                        ; mov rax, [rbx + 8 * callee as i32]
                        ; lea r8, [rbx + 8 * arg_base as i32]
                    );
                    if let Some(t) = this {
                        dynasm!(self.ops
                            ; mov rdx, [rbx + 8 * t as i32]
                            ; mov ecx, (reg_hi | 0x100 | af | argc as i32)
                        );
                    } else {
                        dynasm!(self.ops ; mov ecx, (reg_hi | af | argc as i32));
                    }
                    dynasm!(self.ops
                        ; mov r10, QWORD stub
                        ; call r10
                        ; mov r10, QWORD crate::codegen::SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; je => dyn_miss
                        ; mov r10, QWORD crate::codegen::CALL_THREW as i64
                        ; cmp rax, r10
                        ; je => bail
                        ; mov [rbx + 8 * dst as i32], rax
                        ; jmp => cross_done
                        ; => dyn_miss
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
                if has_method_lane {
                    let lookup_miss = self.ops.new_dynamic_label();
                    let have_callee = self.ops.new_dynamic_label();
                    let dyn_only = !(has_class_lane && arms);
                    let have_mega = mega_key_id != 0;
                    if let Some(lane) = has_class_lane.then_some(class_guard).flatten() {
                        let guard_miss = if dyn_only && !have_mega {
                            lookup_miss
                        } else {
                            self.ops.new_dynamic_label()
                        };
                        crate::codegen::proto_mem::emit_class_read_guard(&mut self.ops, obj, &lane, guard_miss);
                        dynasm!(self.ops
                            ; mov rax, QWORD lane.value as i64
                            ; jmp => have_callee
                        );
                        if !dyn_only || have_mega {
                            dynasm!(self.ops ; => guard_miss);
                        }
                    } else if let Some(plane) = has_class_lane.then_some(proto_guard).flatten() {
                        let guard_miss = if dyn_only && !have_mega {
                            lookup_miss
                        } else {
                            self.ops.new_dynamic_label()
                        };
                        crate::codegen::proto_mem::emit_proto_read_guard(&mut self.ops, obj, &plane, guard_miss);
                        dynasm!(self.ops ; jmp => have_callee);
                        if !dyn_only || have_mega {
                            dynasm!(self.ops ; => guard_miss);
                        }
                    }
                    if have_mega {
                        let full_addr = self.mega_prop_addr(true);
                        let mega_miss = if dyn_only { lookup_miss } else { self.ops.new_dynamic_label() };
                        dynasm!(self.ops
                            ; mov rdx, [rbx + 8 * obj as i32]
                            ; mov ecx, mega_key_id as i32
                            ; mov r8d, -1
                            ; mov r10, QWORD full_addr
                            ; call r10
                            ; mov r10, QWORD crate::codegen::SELF_CALL_DEOPT as i64
                            ; cmp rax, r10
                            ; je => mega_miss
                            ; mov r10, QWORD crate::codegen::PROP_VIA_IC as i64
                            ; cmp rax, r10
                            ; je => mega_miss
                            ; mov r10, QWORD crate::codegen::PROP_CLASS_GETTER as i64
                            ; cmp rax, r10
                            ; je => mega_miss
                        );
                        if !dyn_only {
                            dynasm!(self.ops
                                ; jmp => have_callee
                                ; => mega_miss
                            );
                        }
                    }
                    if !dyn_only {
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
                        );
                    }
                    dynasm!(self.ops ; => have_callee);
                    if !dyn_only {
                        if let Some(site) = site {
                            dynasm!(self.ops ; mov [rsp + c3 + 48], rax);
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
                            if dyn_ok {
                                dynasm!(self.ops ; mov rax, [rsp + c3 + 48]);
                            }
                        }
                    }
                    if dyn_ok {
                        let stub = self.shared.cross3_dyn as i64;
                        let reg_hi = self.reg_hi();
                        dynasm!(self.ops
                            ; mov rdx, [rbx + 8 * obj as i32]
                            ; lea r8, [rbx + 8 * arg_base as i32]
                            ; mov ecx, (reg_hi | 0x100 | argc as i32)
                            ; mov r10, QWORD stub
                            ; call r10
                            ; mov r10, QWORD crate::codegen::SELF_CALL_DEOPT as i64
                            ; cmp rax, r10
                            ; je => lookup_miss
                            ; mov r10, QWORD crate::codegen::CALL_THREW as i64
                            ; cmp rax, r10
                            ; je => bail
                            ; mov [rbx + 8 * dst as i32], rax
                            ; jmp => cross_done
                        );
                    }
                    dynasm!(self.ops ; => lookup_miss);
                }
            }
            CallLane::None | CallLane::Prop { .. } | CallLane::SetProp { .. } => unreachable!(),
        }
        Some((cross_done, dst))
    }

    /// Emit a slow property read's inline cache probe (`emit_ic_probe`).
    /// On IC hit, restores `r13`/`r14` (if saved) and jumps to `done` with the
    /// result in `[rbx + 8*dst]`. On miss, jumps to `miss_lbl` with `rax` =
    /// receiver bits and `r13`/`r14` still loaded (`versions`/`ic_table`).
    /// Returns `Some((site, direct))` when an IC probe was emitted.
    #[allow(clippy::too_many_arguments)]
    fn prop_ic_probe(
        &mut self,
        obj: u16,
        dst: u16,
        is_call: bool,
        is_apply: bool,
        ip: u32,
        done: DynamicLabel,
        miss_lbl: DynamicLabel,
    ) -> Option<(u32, bool)> {
        use super::super::{emit_ic_probe, IcProbe, JIT_IC_STRIDE, JIT_IC_WAYS};
        let &(site, direct) = self.f.ic_sites.get(&ip)?;
        let _heap = self.f.heap?;
        if is_call || is_apply {
            let stub = if is_apply {
                self.shared.fn_get_apply as i64
            } else {
                self.shared.fn_get_call as i64
            };
            let miss = self.ops.new_dynamic_label();
            dynasm!(self.ops
                ; mov rax, [rbx + 8 * obj as i32]
                ; mov r10, QWORD stub
                ; call r10
                ; mov r10, QWORD crate::codegen::SELF_CALL_DEOPT as i64
                ; cmp rax, r10
                ; je => miss
                ; mov [rbx + 8 * dst as i32], rax
                ; jmp => done
                ; => miss
            );
        }
        let off = (site as usize * JIT_IC_WAYS * JIT_IC_STRIDE) as i32;
        let c3 = self.c3;
        let hit = self.ops.new_dynamic_label();
        if self.save_ic_r13 {
            dynasm!(self.ops ; mov [rsp + c3], r13);
        }
        if self.save_ic_r14 {
            dynasm!(self.ops ; mov [rsp + c3 + 8], r14);
        }
        dynasm!(self.ops
            ; mov r13, [rdi + crate::vm::host_api::JIT_VERSIONS_RAW_OFFSET as i32]
            ; mov r14, [rdi + crate::vm::host_api::JIT_IC_TABLE_RAW_OFFSET as i32]
        );
        emit_ic_probe(&mut self.ops, IcProbe::Get { dst }, obj, off, hit, None, direct);
        dynasm!(self.ops
            ; jmp => miss_lbl
            ; => hit
        );
        if self.save_ic_r13 {
            dynasm!(self.ops ; mov r13, [rsp + c3]);
        }
        if self.save_ic_r14 {
            dynasm!(self.ops ; mov r14, [rsp + c3 + 8]);
        }
        Some((site, direct))
    }

    /// Emit a slow property store's inline cache probe (`emit_ic_probe(IcProbe::Set)`).
    /// On IC hit, restores `r13`/`r14` (if saved) and falls through to `done`.
    /// On miss, jumps to `miss_lbl` with `rax` = receiver bits and `r13`/`r14`
    /// still loaded (`versions`/`ic_table`).
    fn set_prop_ic_probe(
        &mut self,
        obj: u16,
        val: u16,
        ip: u32,
        miss_lbl: DynamicLabel,
    ) -> Option<(u32, bool)> {
        use super::super::{emit_ic_probe, IcProbe, JIT_IC_STRIDE, JIT_IC_WAYS};
        let (site, direct) = *self.f.ic_sites.get(&ip)?;
        let _heap = self.f.heap?;
        let off = (site as usize * JIT_IC_WAYS * JIT_IC_STRIDE) as i32;
        let c3 = self.c3;
        let hit = self.ops.new_dynamic_label();
        if self.save_ic_r13 {
            dynasm!(self.ops ; mov [rsp + c3], r13);
        }
        if self.save_ic_r14 {
            dynasm!(self.ops ; mov [rsp + c3 + 8], r14);
        }
        dynasm!(self.ops
            ; mov r13, [rdi + crate::vm::host_api::JIT_VERSIONS_RAW_OFFSET as i32]
            ; mov r14, [rdi + crate::vm::host_api::JIT_IC_TABLE_RAW_OFFSET as i32]
        );
        emit_ic_probe(
            &mut self.ops,
            IcProbe::Set {
                val,
                gen_lane: self.f.gen_lane,
            },
            obj,
            off,
            hit,
            None,
            direct,
        );
        dynasm!(self.ops
            ; jmp => miss_lbl
            ; => hit
        );
        if self.save_ic_r13 {
            dynasm!(self.ops ; mov r13, [rsp + c3]);
        }
        if self.save_ic_r14 {
            dynasm!(self.ops ; mov r14, [rsp + c3 + 8]);
        }
        Some((site, direct))
    }

    /// `obj.<method>(args)` through Tier C's dedicated helper for it (`m`),
    /// over the window the frame state fills: an answer is stored to the
    /// window's `dst` register and jumps to the returned label; a decline
    /// (another receiver, a user method of the name) falls through, nothing
    /// done. `None` when the helpers are not at hand.
    fn method_intr(&mut self, m: MethodIntr, obj: u16, arg_base: u16, dst: u16, ip: u32) -> Option<DynamicLabel> {
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
            MethodIntr::CharCodeAt | MethodIntr::IndexOf | MethodIntr::Push | MethodIntr::StrUnit(_) => {
                if matches!(m, MethodIntr::CharCodeAt) {
                    super::super::emit_misc::emit_inline_ascii_char_code_at(&mut self.ops, dst, obj, arg_base, done);
                }
                let helper = match m {
                    MethodIntr::IndexOf => heap.str_index_of,
                    MethodIntr::Push => heap.array_push,
                    MethodIntr::StrUnit(h) => h as usize,
                    _ => heap.char_code_at,
                };
                if matches!(m, MethodIntr::Push) {
                    super::super::proto_mem::emit_guard_hot_mirror_clear(&mut self.ops, o, decline);
                } else {
                    super::super::proto_mem::emit_guard_hot_mirror_clear_or_str(&mut self.ops, o, decline);
                }
                dynasm!(self.ops
                    ; mov rcx, rdi
                    ; mov r8, [rbx + a0]
                    ; mov rax, QWORD helper as i64
                    ; call rax
                );
                answer(self);
            }
            MethodIntr::Substring(mode) => {
                super::super::proto_mem::emit_guard_hot_mirror_clear_or_str(&mut self.ops, o, decline);
                dynasm!(self.ops
                    ; mov rcx, rdi
                    ; lea r8, [rbx + a0]
                    ; mov r9d, mode
                    ; mov rax, QWORD heap.str_substring as i64
                    ; call rax
                );
                answer(self);
            }
            MethodIntr::Get | MethodIntr::Has => {
                // 0 = Map/WeakMap.get, 3 = unified Map/WeakMap/Set/WeakSet.has.
                let op: i32 = if matches!(m, MethodIntr::Get) { 0 } else { 3 };
                super::super::proto_mem::emit_guard_hot_mirror_clear(&mut self.ops, o, decline);
                dynasm!(self.ops
                    ; mov rcx, rdi
                    ; mov r8, [rbx + a0]
                    ; mov r9d, op
                    ; mov rax, QWORD heap.coll_lookup as i64
                    ; call rax
                );
                answer(self);
            }
            MethodIntr::Mutate(op, argc) => {
                // `delete` asks as a Set (op 4) when not a Map's.
                super::super::proto_mem::emit_guard_hot_mirror_clear(&mut self.ops, o, decline);
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
                    MethodIntr::Upper => {
                        super::super::proto_mem::emit_guard_hot_mirror_clear_or_str(&mut self.ops, o, decline);
                        heap.str_upper_case as i64
                    }
                    _ => {
                        super::super::proto_mem::emit_guard_hot_mirror_clear(&mut self.ops, o, decline);
                        crate::vm::jit_array_pop as *const () as i64
                    }
                };
                dynasm!(self.ops
                    ; mov rcx, rdi
                    ; mov rax, QWORD helper
                    ; call rax
                );
                answer(self);
            }
            MethodIntr::StrArrBuiltin(packed) => {
                super::super::proto_mem::emit_guard_hot_mirror_clear_or_str(&mut self.ops, o, decline);
                dynasm!(self.ops
                    ; mov rcx, rdi
                    ; lea r8, [rbx + a0]
                    ; mov r9d, packed as i32
                    ; mov rax, QWORD crate::vm::jit_str_method as *const () as i64
                    ; call rax
                );
                answer(self);
            }
            MethodIntr::ArrMutate(op, argc) => {
                super::super::proto_mem::emit_guard_hot_mirror_clear(&mut self.ops, o, decline);
                dynasm!(self.ops
                    ; mov rcx, rdi
                    ; lea r8, [rbx + a0]
                    ; mov r9d, (op as i32) | ((argc as i32) << 8)
                    ; mov rax, QWORD crate::vm::jit_array_mutate as *const () as i64
                    ; call rax
                );
                answer(self);
            }
            MethodIntr::FnCall(argc) => {
                if self.can_cross3_dyn() {
                    let stub = self.shared.fn_call_dyn as i64;
                    let threw = self.add_raw_bail(ip);
                    let reg_hi = self.reg_hi();
                    if argc == 0 {
                        dynasm!(self.ops
                            ; mov rax, [rbx + o]
                            ; mov ecx, reg_hi
                            ; mov r10, QWORD stub
                            ; call r10
                        );
                    } else {
                        dynasm!(self.ops
                            ; mov rax, [rbx + o]
                            ; mov rdx, [rbx + a0]
                            ; lea r8, [rbx + a0 + 8]
                            ; mov ecx, (reg_hi | 0x100 | (argc - 1) as i32)
                            ; mov r10, QWORD stub
                            ; call r10
                        );
                    }
                    dynasm!(self.ops
                        ; mov r10, QWORD deopt
                        ; cmp rax, r10
                        ; je => decline
                        ; mov r10, QWORD crate::codegen::CALL_THREW as i64
                        ; cmp rax, r10
                        ; je => threw
                        ; mov [rbx + d], rax
                        ; jmp => done
                    );
                } else {
                    dynasm!(self.ops ; jmp => decline);
                }
            }
            MethodIntr::FnApply(argc) => {
                if self.can_cross3_dyn() {
                    let guard_stub = self.shared.fn_apply_dyn as i64;
                    let threw = self.add_raw_bail(ip);
                    let reg_hi = self.reg_hi();
                    if argc == 0 {
                        dynasm!(self.ops
                            ; mov rax, [rbx + o]
                            ; mov ecx, reg_hi
                            ; mov r10, QWORD guard_stub
                            ; call r10
                        );
                    } else if argc == 1 {
                        dynasm!(self.ops
                            ; mov rax, [rbx + o]
                            ; mov rdx, [rbx + a0]
                            ; mov ecx, (reg_hi | 0x100)
                            ; mov r10, QWORD guard_stub
                            ; call r10
                        );
                    } else {
                        let arr_stub = self.shared.fn_apply_arr as i64;
                        dynasm!(self.ops
                            ; mov rax, [rbx + o]
                            ; mov rdx, [rbx + a0]
                            ; mov r8, [rbx + a0 + 8]
                            ; mov ecx, reg_hi
                            ; mov r10, QWORD arr_stub
                            ; call r10
                        );
                    }
                    dynasm!(self.ops
                        ; mov r10, QWORD deopt
                        ; cmp rax, r10
                        ; je => decline
                        ; mov r10, QWORD crate::codegen::CALL_THREW as i64
                        ; cmp rax, r10
                        ; je => threw
                        ; mov [rbx + d], rax
                        ; jmp => done
                    );
                } else {
                    dynasm!(self.ops ; jmp => decline);
                }
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
    /// window): a declined op or a throw leaves the body at `ip` via a shared
    /// out-of-line raw bail stub.
    fn leave_on_sentinel(&mut self, ip: u32) {
        let exit = self.add_raw_bail(ip);
        dynasm!(self.ops
            ; mov r10, QWORD crate::codegen::SELF_CALL_DEOPT as i64
            ; cmp rax, r10
            ; je => exit
            ; mov r10, QWORD crate::codegen::CALL_THREW as i64
            ; cmp rax, r10
            ; je => exit
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
    fn stress_check(&mut self, fs: u32) {
        if self.stress {
            let stub = self.use_stub(Some(fs));
            let at = &super::STRESS_COUNTDOWN as *const _ as i64;
            dynasm!(self.ops
                ; mov rax, QWORD at
                ; sub DWORD [rax], 1
                ; jz => stub
            );
        }
    }

    fn imm_boxed_bits(&self, v: V) -> Option<u64> {
        match self.f.insts[v as usize].op {
            Op::ConstTagged(bits) => Some(bits),
            Op::ConstI32(c) => Some(TAG_INT | u64::from(c as u32)),
            Op::ConstBool(b) => Some(TAG_BOOL | u64::from(b)),
            _ => None,
        }
    }

    fn store_state_reg(&mut self, r: u16, v: V) {
        let off = 8 * r as i32;
        if let Some(bits) = self.imm_boxed_bits(v) {
            dynasm!(self.ops
                ; mov rax, QWORD bits as i64
                ; mov [rbx + off], rax
            );
            return;
        }
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

    /// Ensure `(r, v)` is boxed into `[rbx + 8*r]`, reusing `window_known` when
    /// already written earlier in this block.
    fn ensure_window_reg(&mut self, r: u16, v: V) {
        if self.window_known.contains(&(r, v)) {
            return;
        }
        self.store_state_reg(r, v);
        if let Some(slot) = self.window_known.iter_mut().find(|&&mut (kr, _)| kr == r) {
            slot.1 = v;
        } else {
            self.window_known.push((r, v));
        }
    }

    /// Unconditional frame state `s` into the window: skips any `(r, v)` already
    /// boxed into `[rbx + 8*r]` earlier in the current basic block, and records
    /// newly written entries into `window_known`.
    fn write_state(&mut self, s: u32) {
        let f = self.f;
        for &(r, v) in &f.states[s as usize].regs {
            self.ensure_window_reg(r, v);
        }
    }

    /// Conditional slow-path frame state `s` into the window: skips entries
    /// already guaranteed by an earlier unconditional write in this block, and
    /// invalidates any register whose cached value is conditionally overwritten.
    fn write_state_cond(&mut self, s: u32) {
        let f = self.f;
        for &(r, v) in &f.states[s as usize].regs {
            if self.window_known.contains(&(r, v)) {
                continue;
            }
            self.store_state_reg(r, v);
            self.invalidate_window_reg(r);
        }
    }

    /// Full frame state `s` into the window for out-of-line deopt/bail stubs
    /// (never consults or updates per-block `window_known`).
    fn write_state_full(&mut self, s: u32) {
        let f = self.f;
        for &(r, v) in &f.states[s as usize].regs {
            self.store_state_reg(r, v);
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
        let fs = i.fs;
        if let Some(s) = fs {
            self.stress_check(s);
        }
        match i.op.clone() {
            Op::Phi(_) | Op::ConstI32(_) | Op::ConstBool(_) | Op::ConstTagged(_) => {}
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
                if matches!(self.a.loc[v as usize], Loc::Param(_)) {
                    return;
                }
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
                let stub = self.use_stub(fs);
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
                let stub = self.use_stub(fs);
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
                    let stub = self.use_stub(fs);
                    dynasm!(self.ops ; jo => stub);
                }
                dynasm!(self.ops ; mov Rd(d), eax);
                self.done_gpr(d, st);
            }
            Op::AddIK(a, k, wrap) => {
                let ra = self.gpr(a, RDX);
                let (d, st) = self.dst_gpr(v);
                dynasm!(self.ops
                    ; mov eax, Rd(ra)
                    ; add eax, k
                );
                if !wrap {
                    let stub = self.use_stub(fs);
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
                let stub = self.use_stub(fs);
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
                let cold = self.ops.new_dynamic_label();
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
                    // Both `null` (`0x7FFB_0000_0000_0000`) and `SmallBigInt(0)`
                    // (`0x7FFB_8000_0000_0000`) have all 47 payload bits zero;
                    // every non-zero `SmallBigInt` has a non-zero 47-bit payload.
                    ; mov rax, Rq(ra)
                    ; shl rax, 17
                    ; jz => falsy
                    ; jmp => truthy
                    // A heap value (an empty string, a zero BigInt and the
                    // [[IsHTMLDDA]] object are falsy): fast zero-DRAM check
                    // first (no register saves or call!), else out-of-line
                    // `truthy_stub` with volatile allocator registers preserved.
                    ; => heap
                    ; mov rax, Rq(ra)
                    ; cmp BYTE [rdi + crate::vm::host_api::JIT_HTMLDDA_SCALAR_ENABLED_OFFSET as i32], 0
                    ; je => cold
                    ; mov ecx, eax
                    ; cmp ecx, DWORD [rdi + crate::vm::host_api::JIT_HTMLDDA_IDX_OFFSET as i32]
                    ; je => falsy
                    ; cmp ecx, crate::heap::INTERN_EMPTY as i32
                    ; je => falsy
                    ; cmp BYTE [rdi + crate::vm::host_api::JIT_HAS_NON_INTERNED_FALSY_OFFSET as i32], 0
                    ; jne => cold
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
                self.deferred_cold.push(ColdTask::TaggedTruthy {
                    lbl: cold,
                    a,
                    truthy,
                    falsy,
                });
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
                let stub = self.use_stub(fs);
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
                let stub = self.use_stub(fs);
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
            Op::LoadGlobalSlow { idx, ip } => {
                let s = i.fs.expect("LoadGlobalSlow has a frame state");
                let bail = self.add_plain_bail(s, ip);
                dynasm!(self.ops
                    ; mov rcx, rdi
                    ; mov edx, idx as i32
                    ; mov rax, QWORD crate::vm::jit_load_global_slow as *const () as i64
                    ; call rax
                    ; mov r10, QWORD crate::codegen::SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                );
                let (d, st) = self.dst_gpr(v);
                dynasm!(self.ops ; mov Rq(d), rax);
                self.done_gpr(d, st);
            }
            Op::LoadGlobalOrUndefSlow { idx, ip } => {
                let s = i.fs.expect("LoadGlobalOrUndefSlow has a frame state");
                let bail = self.add_plain_bail(s, ip);
                dynasm!(self.ops
                    ; mov rcx, rdi
                    ; mov edx, idx as i32
                    ; mov rax, QWORD crate::vm::jit_load_global_or_undef as *const () as i64
                    ; call rax
                    ; mov r10, QWORD crate::codegen::SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                );
                let (d, st) = self.dst_gpr(v);
                dynasm!(self.ops ; mov Rq(d), rax);
                self.done_gpr(d, st);
            }
            Op::GuardConstObj { obj, oidx, ver } => {
                let stub = self.use_stub(fs);
                let ro = self.gpr(obj, RDX);
                let obj_bits = Value::heap(oidx).bits();
                let versions = crate::vm::host_api::JIT_VERSIONS_RAW_OFFSET as i32;
                dynasm!(self.ops
                    ; mov rax, QWORD obj_bits as i64
                    ; cmp Rq(ro), rax
                    ; jne => stub
                    ; mov r11, [rdi + versions]
                    ; cmp DWORD [r11 + (oidx as i32) * 4], ver as i32
                    ; jne => stub
                );
            }
            Op::GuardFid { v: x, fid } => {
                let stub = self.use_stub(fs);
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
                let stub = self.use_stub(fs);
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
                if let CallLane::SetProp {
                    obj,
                    obj_v,
                    val,
                    val_v,
                    name,
                } = lane
                {
                    if self.f.ic_sites.contains_key(&ip) && self.f.heap.is_some() {
                        self.ensure_window_reg(obj, obj_v);
                        self.ensure_window_reg(val, val_v);
                        let miss = self.ops.new_dynamic_label();
                        let done = self.ops.new_dynamic_label();
                        let (ic_site, ic_direct) = self
                            .set_prop_ic_probe(obj, val, ip, miss)
                            .expect("checked ic_sites and heap");
                        dynasm!(self.ops ; => done);
                        self.deferred_cold.push(ColdTask::CallOutSetPropMiss {
                            lbl: miss,
                            done,
                            val,
                            name,
                            s,
                            ip,
                            packed,
                            helper,
                            argc,
                            ic_site,
                            ic_direct,
                        });
                        self.invalidate_window_state(s);
                        return;
                    }
                    self.write_state(s);
                    self.call_written(helper, ip, packed, argc);
                    return;
                }
                if let CallLane::Prop {
                    obj,
                    obj_v,
                    dst,
                    name,
                    is_call,
                    is_apply,
                    str_char_code_at,
                    str_slot_ptr,
                    arr_slot_ptr,
                    kind_idx,
                } = lane
                {
                    if let Some(heap) = self.f.heap {
                        self.ensure_window_reg(obj, obj_v);
                        let done = self.ops.new_dynamic_label();
                        let join = self.ops.new_dynamic_label();
                        let miss = self.ops.new_dynamic_label();
                        if let Some(clane) = self.f.class_read.get(&ip).copied() {
                            super::super::proto_mem::emit_class_read_get(&mut self.ops, obj, dst, &clane, done);
                        }
                        if str_char_code_at {
                            super::super::emit_misc::emit_inline_str_prop_char_code_at(&mut self.ops, obj, dst, done);
                        } else if str_slot_ptr != 0 && heap.str_proto_ver != u32::MAX {
                            super::super::emit_misc::emit_inline_str_proto_prop(
                                &mut self.ops,
                                obj,
                                dst,
                                heap.str_proto_ver,
                                str_slot_ptr,
                                done,
                            );
                        }
                        if arr_slot_ptr != 0 && heap.arr_proto_ver != u32::MAX {
                            super::super::emit_misc::emit_inline_arr_proto_prop(
                                &mut self.ops,
                                obj,
                                dst,
                                heap.arr_proto_ver,
                                arr_slot_ptr,
                                done,
                            );
                        }
                        if let Some(ki) = kind_idx {
                            super::super::emit_misc::emit_inline_kind_proto_prop(
                                &mut self.ops,
                                obj,
                                dst,
                                ki as usize,
                                &heap.kind_proto_vers,
                                &heap.kind_proto_slot_ptrs,
                                done,
                            );
                        }
                        let ic_miss = self.prop_ic_probe(obj, dst, is_call, is_apply, ip, done, miss);
                        if ic_miss.is_none() {
                            dynasm!(self.ops ; jmp => miss);
                        }
                        dynasm!(self.ops
                            ; => done
                            ; mov rax, [rbx + 8 * dst as i32]
                            ; => join
                        );
                        self.deferred_cold.push(ColdTask::CallOutPropMiss {
                            lbl: miss,
                            join,
                            obj,
                            name,
                            s,
                            ip,
                            packed,
                            helper,
                            argc,
                            ic_miss,
                        });
                        self.invalidate_window_state(s);
                        self.invalidate_window_reg(dst);
                        if i.rep != Rep::Void {
                            let (d, st) = self.dst_gpr(v);
                            dynasm!(self.ops ; mov Rq(d), rax);
                            self.done_gpr(d, st);
                        }
                        return;
                    }
                    self.write_state(s);
                    self.call_written(helper, ip, packed, argc);
                    if i.rep != Rep::Void {
                        let (d, st) = self.dst_gpr(v);
                        dynasm!(self.ops ; mov Rq(d), rax);
                        self.done_gpr(d, st);
                    }
                    return;
                }
                let pure_method = matches!(
                    lane,
                    CallLane::Method { intr: Some(m), .. } if m.is_pure_no_gc()
                );
                // The lanes ahead of the helper, which leave their result in
                // the window's `dst` register.
                let mut made: Vec<(DynamicLabel, u16)> = Vec::new();
                let mut wrote_state_inline = false;
                if pure_method {
                    if let CallLane::Method {
                        obj,
                        obj_v,
                        arg_base,
                        arg0_v,
                        dst,
                        intr: Some(m),
                        ..
                    } = lane
                    {
                        self.ensure_window_reg(obj, obj_v);
                        if let Some(a0) = arg0_v {
                            self.ensure_window_reg(arg_base, a0);
                        }
                        if let Some(l) = self.method_intr(m, obj, arg_base, dst, ip) {
                            made.push((l, dst));
                        }
                    }
                    let has_cross = self.f.heap.is_some() && {
                        let site = self.f.cross.get(&ip).copied();
                        let dyn_ok = self.can_cross3_dyn() && argc <= 255;
                        let own = site
                            .and_then(|s| s.cross3m)
                            .filter(|_| super::super::tierc_cross3m_enabled());
                        let has_class = site.is_some_and(|s| s.class_method)
                            && (site.is_some_and(|s| !s.cross3.is_empty() && (!dyn_ok || s.cross3.len() <= 1))
                                || (dyn_ok && (self.f.class_read.contains_key(&ip) || site.and_then(|s| s.proto_read).is_some())));
                        let has_mega = matches!(lane, CallLane::Method { mega_ok: true, .. })
                            && dyn_ok
                            && !crate::vm::icstats_enabled()
                            && self.prop_key_id((packed >> 32) as u32) != 0;
                        own.is_some() || has_class || has_mega
                    };
                    if has_cross {
                        self.write_state_cond(s);
                        wrote_state_inline = true;
                        made.extend(self.cross_lanes(lane, ip, packed, argc));
                    }
                } else {
                    self.write_state(s);
                    wrote_state_inline = true;
                    if let CallLane::Method {
                        obj,
                        arg_base,
                        dst,
                        intr: Some(m),
                        ..
                    } = lane
                    {
                        if let Some(l) = self.method_intr(m, obj, arg_base, dst, ip) {
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
                            CapturedLane::Direct { bits, helper, is_char_code_at, .. } => {
                                emit_captured_builtin_lane(&mut self.ops, callee, this_v, arg_base, dst, bits, helper, is_char_code_at, None)
                            }
                            CapturedLane::Proved { op } => {
                                emit_captured_method_lane(&mut self.ops, callee, this_v, arg_base, argc, dst, op, None, heap)
                            }
                        };
                        made.push((l, dst));
                    }
                    made.extend(self.cross_lanes(lane, ip, packed, argc));
                }
                if let Some(&(_, dst)) = made.first() {
                    let miss = self.ops.new_dynamic_label();
                    let join = self.ops.new_dynamic_label();
                    dynasm!(self.ops ; jmp => miss);
                    for &(l, _) in &made {
                        dynasm!(self.ops ; => l);
                    }
                    dynasm!(self.ops
                        ; mov rax, [rbx + 8 * dst as i32]
                        ; => join
                    );
                    self.deferred_cold.push(ColdTask::CallOutFallback {
                        lbl: miss,
                        join,
                        write_s: (!wrote_state_inline).then_some(s),
                        helper,
                        ip,
                        packed,
                        argc,
                    });
                    if !wrote_state_inline {
                        self.invalidate_window_state(s);
                    }
                    self.invalidate_window_reg(dst);
                } else {
                    if !wrote_state_inline {
                        self.write_state(s);
                    }
                    self.call_written(helper, ip, packed, argc);
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
                if let Some(ready) = ready {
                    let miss = self.ops.new_dynamic_label();
                    let join = self.ops.new_dynamic_label();
                    dynasm!(self.ops
                        ; jmp => miss
                        ; => ready
                        ; mov rax, [rbx + 8 * dst as i32]
                        ; => join
                    );
                    self.deferred_cold.push(ColdTask::LiteralSlow {
                        lbl: miss,
                        join,
                        plan_ptr,
                        packed,
                        ip,
                    });
                    self.invalidate_window_reg(dst);
                } else {
                    dynasm!(self.ops
                        ; mov rcx, rdi
                        ; mov rdx, rbx
                        ; mov r8, QWORD plan_ptr as i64
                        ; mov r9, QWORD packed as i64
                        ; mov rax, QWORD crate::vm::jit_finalize_object_thin as *const () as i64
                        ; call rax
                    );
                    self.leave_on_sentinel(ip);
                }
                let (d, st) = self.dst_gpr(v);
                dynasm!(self.ops ; mov Rq(d), rax);
                self.done_gpr(d, st);
            }
            Op::IterPure { helper, src } => {
                let stub = self.use_stub(fs);
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
                    let miss = self.ops.new_dynamic_label();
                    crate::codegen::emit_arr_mirror_iter_step(&mut self.ops, value_dst, done_dst, iter, idx, stepped);
                    dynasm!(self.ops ; jmp => miss ; => stepped);
                    self.deferred_cold.push(ColdTask::IterStepSlow {
                        lbl: miss,
                        done: stepped,
                        packed,
                        idx,
                        ip,
                    });
                } else {
                    dynasm!(self.ops
                        ; mov rcx, rdi
                        ; mov rdx, rbx
                        ; mov r8, QWORD packed as i64
                        ; mov r9d, idx as i32
                        ; mov rax, QWORD heap.iter_next as i64
                        ; call rax
                    );
                    self.leave_on_sentinel(ip);
                }
                self.invalidate_window_reg(value_dst);
                self.invalidate_window_reg(done_dst);
                self.invalidate_window_reg(idx);
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
                self.invalidate_window_reg(r);
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
                if i.rep != Rep::Void {
                    let (d, st) = self.dst_gpr(v);
                    dynasm!(self.ops ; mov Rq(d), rax);
                    self.done_gpr(d, st);
                } else {
                    // Void AllocOut (`jit_make_cell`) writes the new cell into `[rbx + 8*reg]`.
                    self.window_known.clear();
                }
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
                // the instance made in compiled code, else out-of-line `jit_new_ic`.
                let s = i.fs.expect("a `new` has a frame state");
                self.write_state(s);
                let made = super::super::emit_misc::emit_new_lane(&mut self.ops, lane, callee, arg_base, dst);
                let miss = self.ops.new_dynamic_label();
                let join = self.ops.new_dynamic_label();
                dynasm!(self.ops
                    ; jmp => miss
                    ; => made
                    ; mov rax, [rbx + 8 * dst as i32]
                    ; => join
                );
                self.deferred_cold.push(ColdTask::NewLaneSlow {
                    lbl: miss,
                    join,
                    ip,
                    packed,
                    argc,
                });
                self.invalidate_window_reg(dst);
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
                let barrier = self.ops.new_dynamic_label();
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
                    ; mov rcx, rax
                    ; shr rcx, 48
                    ; cmp ecx, 0x7FFD
                    ; je => barrier
                    ; => done
                );
                self.deferred_cold.push(ColdTask::StoreBarrierNoVol {
                    lbl: barrier,
                    done,
                    obj: Some(obj),
                });
                self.deferred_cold.push(ColdTask::SetPropSlow {
                    lbl: miss,
                    done,
                    obj,
                    val,
                    s,
                    ip,
                    packed,
                });
                self.invalidate_window_state(s);
            }
            Op::UpvalGet(idx) => {
                let stub = self.use_stub(fs);
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
                let stub = self.use_stub(fs);
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
                let stub = self.use_stub(fs);
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
                let stub = self.use_stub(fs);
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
                    ; => done
                );
                self.deferred_cold.push(ColdTask::ModFSlow {
                    lbl: slow,
                    done,
                    a,
                    b,
                    d,
                });
                self.done_xmm(d, st);
            }
            Op::ShapeVals { obj, shape } => {
                let stub = self.use_stub(fs);
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
                let barrier = self.ops.new_dynamic_label();
                let done = self.ops.new_dynamic_label();
                dynasm!(self.ops
                    ; mov [Rq(rv) + 8 * slot as i32], Rq(rx)
                    // A primitive needs no barrier.
                    ; mov rax, Rq(rx)
                    ; mov rcx, rax
                    ; shr rcx, 48
                    ; cmp ecx, 0x7FFD
                    ; je => barrier
                    ; => done
                );
                self.deferred_cold.push(ColdTask::StoreBarrierNoVol {
                    lbl: barrier,
                    done,
                    obj: Some(obj),
                });
            }
            Op::GetPropShape { obj, shape, slot, slow } => {
                let ro = self.gpr(obj, RDX);
                let (d, st) = self.dst_gpr(v);
                let hot_off = crate::vm::host_api::JIT_HOT_MIRROR_RAW_OFFSET as i32;
                let hot_vals = crate::vm::host_api::JIT_HOT_VALS_OFF as i32;
                let off = 8 * slot as i32;
                let miss = if slow.is_some() {
                    self.ops.new_dynamic_label()
                } else {
                    self.use_stub(fs)
                };
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
                    let s = i.fs.expect("a read has a frame state");
                    let done = self.ops.new_dynamic_label();
                    dynasm!(self.ops ; => done);
                    self.deferred_cold.push(ColdTask::GetPropSlow {
                        lbl: miss,
                        done,
                        obj,
                        d,
                        s,
                        ip,
                        packed,
                    });
                    self.invalidate_window_state(s);
                }
                self.done_gpr(d, st);
            }
            Op::Length { obj, ip, packed } => {
                let s = i.fs.expect("a read has a frame state");
                let ro = self.gpr(obj, RDX);
                let (d, st) = self.dst_gpr(v);
                let arr_raw = crate::vm::host_api::JIT_ARR_MIRROR_RAW_OFFSET as i32;
                let arr_len = crate::vm::host_api::JIT_ARR_MIRROR_LEN_OFFSET as i32;
                let hot_raw = crate::vm::host_api::JIT_HOT_MIRROR_RAW_OFFSET as i32;
                let hot_len = crate::vm::host_api::JIT_HOT_MIRROR_LEN_OFFSET as i32;
                let hot_fid = crate::vm::host_api::JIT_HOT_FID_OFF as i32;
                let upvals_raw = crate::vm::host_api::JIT_UPVALS_MIRROR_RAW_OFFSET as i32;
                let this_raw = crate::vm::host_api::JIT_THIS_MIRROR_RAW_OFFSET as i32;
                let (str_chk, empty_str, slow, done) = (
                    self.ops.new_dynamic_label(),
                    self.ops.new_dynamic_label(),
                    self.ops.new_dynamic_label(),
                    self.ops.new_dynamic_label(),
                );
                dynasm!(self.ops
                    // Fast inline `.length`: dense Array (`arr_mirror`), empty
                    // string (`INTERN_EMPTY`), or pinned flat String (`hot_mirror`
                    // + `upvals_mirror != 0` -> `this_mirror[idx]`).
                    ; mov rax, Rq(ro)
                    ; shr rax, 48
                    ; cmp eax, 0x7FFD
                    ; jne => slow
                    ; mov ecx, Rd(ro)
                    ; cmp ecx, [rdi + arr_len]
                    ; jae => str_chk
                    ; mov r10, [rdi + arr_raw]
                    ; mov r11d, ecx
                    ; shl r11, 4
                    ; add r10, r11
                    ; cmp QWORD [r10], 0
                    ; je => str_chk
                    ; mov r10, [r10 + 8]
                    ; cmp r10, i32::MAX
                    ; ja => slow
                    ; mov rax, QWORD TAG_INT as i64
                    ; or rax, r10
                    ; mov Rq(d), rax
                    ; jmp => done
                    ; => str_chk
                    ; cmp ecx, crate::heap::INTERN_EMPTY as i32
                    ; je => empty_str
                    ; cmp ecx, [rdi + hot_len]
                    ; jae => slow
                    ; mov r10, [rdi + hot_raw]
                    ; mov r11d, ecx
                    ; shl r11, 4
                    ; cmp DWORD [r10 + r11 + hot_fid], crate::heap::FID_MIRROR_STR as i32
                    ; jne => slow
                    ; mov r10, [rdi + upvals_raw]
                    ; cmp QWORD [r10 + rcx * 8], 0
                    ; je => slow
                    ; mov r10, [rdi + this_raw]
                    ; mov r10, [r10 + rcx * 8]
                    ; mov rax, QWORD TAG_INT as i64
                    ; or rax, r10
                    ; mov Rq(d), rax
                    ; jmp => done
                    ; => empty_str
                    ; mov Rq(d), QWORD TAG_INT as i64
                    ; => done
                );
                self.deferred_cold.push(ColdTask::LengthSlow {
                    lbl: slow,
                    done,
                    obj,
                    d,
                    s,
                    ip,
                    packed,
                });
                self.invalidate_window_state(s);
                self.done_gpr(d, st);
            }
            Op::CharCodeAt { obj, idx } => {
                use crate::vm::host_api::{
                    JIT_HOT_FID_OFF, JIT_HOT_MIRROR_RAW_OFFSET, JIT_REALM_GLOBALS_NONEMPTY_OFFSET,
                    JIT_STR_PROTO_CCA_FN_BITS_OFFSET, JIT_STR_PROTO_CCA_SLOT_PTR_OFFSET,
                    JIT_STR_PROTO_CCA_VER_OFFSET, JIT_STR_PROTO_IDX_OFFSET,
                    JIT_THIS_MIRROR_RAW_OFFSET, JIT_UPVALS_MIRROR_RAW_OFFSET,
                    JIT_VERSIONS_RAW_OFFSET,
                };
                let stub = self.use_stub(fs);
                let (d, st) = self.dst_gpr(v);
                let slow = self.ops.new_dynamic_label();
                let done = self.ops.new_dynamic_label();
                self.index_to_rax(idx, slow);
                let ro = self.gpr(obj, RDX);
                dynasm!(self.ops
                    ; mov r10, Rq(ro)
                    ; shr r10, 48
                    ; cmp r10d, 0x7FFD
                    ; jne => slow
                    ; mov ecx, Rd(ro)
                    ; cmp BYTE [rdi + JIT_REALM_GLOBALS_NONEMPTY_OFFSET as i32], 0
                    ; jne => slow
                    ; mov r11, [rdi + JIT_STR_PROTO_CCA_SLOT_PTR_OFFSET as i32]
                    ; test r11, r11
                    ; jz => slow
                    ; mov edx, DWORD [rdi + JIT_STR_PROTO_IDX_OFFSET as i32]
                    ; mov r10, [rdi + JIT_VERSIONS_RAW_OFFSET as i32]
                    ; mov edx, DWORD [r10 + rdx * 4]
                    ; cmp edx, DWORD [rdi + JIT_STR_PROTO_CCA_VER_OFFSET as i32]
                    ; jne => slow
                    ; mov r10, [r11]
                    ; cmp r10, [rdi + JIT_STR_PROTO_CCA_FN_BITS_OFFSET as i32]
                    ; jne => slow
                    ; mov r10, [rdi + JIT_HOT_MIRROR_RAW_OFFSET as i32]
                    ; lea r10, [r10 + rcx * 8]
                    ; cmp DWORD [r10 + rcx * 8 + JIT_HOT_FID_OFF as i32], crate::heap::FID_MIRROR_STR as i32
                    ; jne => slow
                    ; mov r11, [rdi + JIT_UPVALS_MIRROR_RAW_OFFSET as i32]
                    ; mov r11, [r11 + rcx * 8]
                    ; cmp r11, 1
                    ; jbe => slow
                    ; mov r10, [rdi + JIT_THIS_MIRROR_RAW_OFFSET as i32]
                    ; cmp rax, [r10 + rcx * 8]
                    ; jae => slow
                    ; movzx eax, BYTE [r11 + rax]
                    ; mov r10, QWORD TAG_INT as i64
                    ; or rax, r10
                    ; mov Rq(d), rax
                    ; => done
                );
                self.deferred_cold.push(ColdTask::CharCodeAtSlow {
                    lbl: slow,
                    done,
                    obj,
                    idx,
                    d,
                    stub,
                });
                self.done_gpr(d, st);
            }
            Op::GetPropPoly { obj, ways, slow } => {
                let s = i.fs.expect("a read has a frame state");
                let ro = self.gpr(obj, RDX);
                let (d, st) = self.dst_gpr(v);
                let hot_off = crate::vm::host_api::JIT_HOT_MIRROR_RAW_OFFSET as i32;
                let hot_vals = crate::vm::host_api::JIT_HOT_VALS_OFF as i32;
                let miss = if slow.is_some() {
                    self.ops.new_dynamic_label()
                } else {
                    self.use_stub(fs)
                };
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
                for (k, (&(_, slot), &arm)) in ways.iter().zip(&arms).enumerate() {
                    dynasm!(self.ops
                        ; => arm
                        ; mov r10, [r11 + rcx*8 + hot_vals]
                        ; mov Rq(d), [r10 + 8 * slot as i32]
                    );
                    if k + 1 < arms.len() {
                        dynasm!(self.ops ; jmp => done);
                    }
                }
                dynasm!(self.ops ; => done);
                if let Some((ip, packed)) = slow {
                    self.deferred_cold.push(ColdTask::GetPropSlow {
                        lbl: miss,
                        done,
                        obj,
                        d,
                        s,
                        ip,
                        packed,
                    });
                    self.invalidate_window_state(s);
                }
                self.done_gpr(d, st);
            }
            Op::AddT(a, b) => {
                let s = i.fs.expect("a `+` has a frame state");
                let ip = f.states[s as usize].ip;
                #[cfg(not(feature = "safe-sandbox"))]
                {
                    use crate::vm::host_api::{
                        JIT_CONCAT_MEMO_RAW_OFFSET, JIT_GC_REQUESTED_OFFSET, JIT_GC_STRESS_OFFSET,
                        JIT_HOT_MIRROR_LEN_OFFSET, JIT_VERSIONS_RAW_OFFSET,
                    };
                    let miss = self.ops.new_dynamic_label();
                    let done = self.ops.new_dynamic_label();
                    let dst_reg = (i.rep != Rep::Void).then(|| self.dst_gpr(v));
                    // Register-only inline `concat_memo` probe (touches only
                    // `rax, rcx, rdx, r10, r11`, preserving all allocator regs
                    // for out-of-line `write_state_full(s)` on miss).
                    dynasm!(self.ops
                        ; cmp BYTE [rdi + JIT_GC_REQUESTED_OFFSET as i32], 0
                        ; jne => miss
                        ; cmp BYTE [rdi + JIT_GC_STRESS_OFFSET as i32], 0
                        ; jne => miss
                        ; mov r11, [rdi + JIT_CONCAT_MEMO_RAW_OFFSET as i32]
                        ; test r11, r11
                        ; jz => miss
                    );
                    let ra = self.gpr(a, RDX);
                    dynasm!(self.ops
                        ; mov rax, Rq(ra)
                        ; mov r10, rax
                        ; shr r10, 48
                        ; cmp r10d, 0x7FFD
                        ; jne => miss
                    );
                    let rb = self.gpr(b, RDX);
                    dynasm!(self.ops
                        ; mov rcx, Rq(rb)
                        ; mov r10, rcx
                        ; shr r10, 48
                        ; cmp r10d, 0x7FF9
                        ; jne => miss
                        ; mov eax, eax
                        ; cmp eax, [rdi + JIT_HOT_MIRROR_LEN_OFFSET as i32]
                        ; jae => miss
                        ; imul r10d, ecx, DWORD -1640531535
                        ; xor r10d, eax
                        ; and r10d, 2047
                        ; lea r10, [r10 + r10 * 4]
                        ; shl r10, 2
                        ; add r11, r10
                        ; cmp [r11], eax
                        ; jne => miss
                        ; cmp [r11 + 8], ecx
                        ; jne => miss
                        ; mov rcx, [rdi + JIT_VERSIONS_RAW_OFFSET as i32]
                        ; mov edx, [r11 + 4]
                        ; cmp edx, [rcx + rax * 4]
                        ; jne => miss
                        ; mov eax, [r11 + 12]
                        ; cmp eax, [rdi + JIT_HOT_MIRROR_LEN_OFFSET as i32]
                        ; jae => miss
                        ; mov edx, [r11 + 16]
                        ; cmp edx, [rcx + rax * 4]
                        ; jne => miss
                        ; mov r10, QWORD crate::codegen::HEAP_TAG as i64
                        ; or rax, r10
                    );
                    if let Some((d, _)) = dst_reg {
                        dynasm!(self.ops ; mov Rq(d), rax);
                    }
                    dynasm!(self.ops ; => done);
                    self.deferred_cold.push(ColdTask::AddTSlow {
                        lbl: miss,
                        done,
                        a,
                        b,
                        dst: dst_reg.map(|(d, _)| d),
                        s,
                        ip,
                    });
                    self.invalidate_window_state(s);
                    if let Some((d, st)) = dst_reg {
                        self.done_gpr(d, st);
                    }
                }
                #[cfg(feature = "safe-sandbox")]
                {
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
                    if i.rep != Rep::Void {
                        let (d, st) = self.dst_gpr(v);
                        dynasm!(self.ops ; mov Rq(d), rax);
                        self.done_gpr(d, st);
                    }
                }
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
                    // anything else through out-of-line `jit_tierc_upval_set`.
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
                    ; => done
                );
                self.deferred_cold.push(ColdTask::UpvalSetSlow {
                    lbl: slow,
                    done,
                    idx,
                    x,
                    s,
                });
                self.invalidate_window_state(s);
            }
            Op::GetIndex {
                obj,
                key,
                ip,
                packed,
                ta,
                inlined,
            } => {
                let s = i.fs.expect("a read has a frame state");
                let (d, st) = self.dst_gpr(v);
                let slow = self.ops.new_dynamic_label();
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
                );
                let mut ta_fill = None;
                if let Some((rec, lane)) = ta_lane {
                    let fill = self.ops.new_dynamic_label();
                    ta_fill = Some((fill, rec));
                    let stub_ta = self.shared.ta_read as i64;
                    dynasm!(self.ops
                        ; jmp => done
                        ; => lane
                    );
                    self.ta_record_check(ro, rec, fill, slow);
                    let vs = self.vol_save;
                    dynasm!(self.ops
                        ; cmp rax, r11
                        ; jae => slow
                        ; mov rdx, rax
                    );
                    if self.uses_r9 {
                        dynasm!(self.ops ; mov [rsp + vs + 8], r9);
                    }
                    dynasm!(self.ops
                        ; mov r9, [rsp + rec + 8]
                        ; movzx ecx, BYTE [rsp + rec + 24]
                        ; mov r10, QWORD stub_ta
                        ; call r10
                    );
                    if self.uses_r9 {
                        dynasm!(self.ops ; mov r9, [rsp + vs + 8]);
                    }
                    dynasm!(self.ops ; mov Rq(d), rax);
                }
                dynasm!(self.ops ; => done);
                self.deferred_cold.push(ColdTask::GetIndexSlow {
                    lbl: slow,
                    done,
                    ta_fill,
                    obj,
                    key,
                    d,
                    s,
                    ip,
                    packed,
                    inlined,
                });
                if !inlined {
                    self.invalidate_window_state(s);
                }
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
                let barrier = self.ops.new_dynamic_label();
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
                    ; mov rcx, rax
                    ; shr rcx, 48
                    ; cmp ecx, 0x7FFD
                    ; je => barrier                       // heap value: out-of-line barrier
                );
                let mut ta_fill = None;
                if let Some((rec, lane)) = ta_lane {
                    let fill = self.ops.new_dynamic_label();
                    ta_fill = Some((fill, rec));
                    let stub_ta = self.shared.ta_write as i64;
                    let vs = self.vol_save;
                    dynasm!(self.ops
                        ; jmp => done
                        ; => lane
                    );
                    let ro = self.gpr(obj, RDX);
                    self.ta_record_check(ro, rec, fill, slow);
                    dynasm!(self.ops
                        ; cmp BYTE [rsp + rec + 25], 0
                        ; je => slow
                        ; cmp rax, r11
                        ; jae => slow
                    );
                    if self.uses_r8 {
                        dynasm!(self.ops ; mov [rsp + vs], r8);
                    }
                    if self.uses_r9 {
                        dynasm!(self.ops ; mov [rsp + vs + 8], r9);
                    }
                    let rv = self.gpr(val, 8);
                    dynasm!(self.ops
                        ; mov r8, Rq(rv)
                        ; mov rdx, rax
                        ; mov r9, [rsp + rec + 8]
                        ; movzx ecx, BYTE [rsp + rec + 24]
                        ; mov r10, QWORD stub_ta
                        ; call r10
                    );
                    if self.uses_r8 {
                        dynasm!(self.ops ; mov r8, [rsp + vs]);
                    }
                    if self.uses_r9 {
                        dynasm!(self.ops ; mov r9, [rsp + vs + 8]);
                    }
                    dynasm!(self.ops
                        ; test eax, eax
                        ; jz => slow
                    );
                }
                dynasm!(self.ops ; => done);
                self.deferred_cold.push(ColdTask::StoreBarrierNoVol {
                    lbl: barrier,
                    done,
                    obj: Some(obj),
                });
                self.deferred_cold.push(ColdTask::SetIndexSlow {
                    lbl: slow,
                    done,
                    ta_fill,
                    obj,
                    key,
                    val,
                    s,
                    ip,
                    packed,
                });
                self.invalidate_window_state(s);
            }
            Op::CmpT(c, x, y) => {
                let stub = self.use_stub(fs);
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
                    ; => done
                );
                self.deferred_cold.push(ColdTask::CmpTSlow {
                    lbl: strs,
                    done,
                    op,
                    d,
                    stub,
                });
                self.done_gpr(d, st);
            }
            Op::ClassConst { bits, epoch } => {
                let stub = self.use_stub(fs);
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
                let stub = self.shared.strict_eq as i64;
                let (diff, do_stub, from_stub, is_eq, is_ne_lbl, done) = (
                    self.ops.new_dynamic_label(),
                    self.ops.new_dynamic_label(),
                    self.ops.new_dynamic_label(),
                    self.ops.new_dynamic_label(),
                    self.ops.new_dynamic_label(),
                    self.ops.new_dynamic_label(),
                );
                dynasm!(self.ops
                    ; mov rax, Rq(ra)
                    ; mov rcx, Rq(rb)
                    ; mov rdx, rax
                    ; shr rdx, 48
                    ; sub edx, 0x7FF9
                    ; cmp rax, rcx
                    ; jne => diff
                    ; cmp edx, 4
                    ; jbe => is_eq
                    ; mov r10, QWORD stub
                    ; call r10
                    ; jmp => from_stub
                    ; => diff
                    ; cmp edx, 4
                    ; ja => do_stub
                    ; mov r8, rcx
                    ; shr r8, 48
                    ; sub r8d, 0x7FF9
                    ; cmp r8d, 4
                    ; ja => do_stub
                    ; and edx, r8d
                    ; test dl, 4
                    ; jz => is_ne_lbl
                    ; => do_stub
                    ; mov r10, QWORD stub
                    ; call r10
                    ; => from_stub
                );
                if !eq {
                    dynasm!(self.ops ; xor eax, 1);
                }
                dynasm!(self.ops
                    ; mov Rd(d), eax
                    ; jmp => done
                    ; => is_eq
                    ; mov Rd(d), i32::from(eq)
                    ; jmp => done
                    ; => is_ne_lbl
                    ; mov Rd(d), i32::from(!eq)
                    ; => done
                );
                self.done_gpr(d, st);
            }
            Op::StrictEqBits(eq, a, bits) => {
                let ra = self.gpr(a, RDX);
                let (d, st) = self.dst_gpr(v);
                dynasm!(self.ops
                    ; mov rcx, QWORD bits as i64
                    ; cmp Rq(ra), rcx
                );
                if eq {
                    dynasm!(self.ops ; sete al);
                } else {
                    dynasm!(self.ops ; setne al);
                }
                dynasm!(self.ops ; movzx eax, al ; mov Rd(d), eax);
                self.done_gpr(d, st);
            }
            Op::IsAtomOrPrim(a) => {
                let ra = self.gpr(a, RDX);
                let (d, st) = self.dst_gpr(v);
                let yes = self.ops.new_dynamic_label();
                let no = self.ops.new_dynamic_label();
                let done = self.ops.new_dynamic_label();
                dynasm!(self.ops
                    ; mov rax, Rq(ra)
                    ; shr rax, 48
                    ; cmp eax, 0x7FFD
                    ; jne => yes
                    ; mov ecx, Rd(ra)
                    ; mov r10, rcx
                    ; shr r10, 6
                    ; cmp r10, [rdi + crate::vm::host_api::JIT_ATOMS_WORDS_OFFSET as i32]
                    ; jae => no
                    ; mov r11, [rdi + crate::vm::host_api::JIT_ATOMS_RAW_OFFSET as i32]
                    ; mov r10, [r11 + r10 * 8]
                    ; bt r10, rcx
                    ; setc al
                    ; movzx eax, al
                    ; mov Rd(d), eax
                    ; jmp => done
                    ; => yes
                    ; mov Rd(d), 1
                    ; jmp => done
                    ; => no
                    ; xor Rd(d), Rd(d)
                    ; => done
                );
                self.done_gpr(d, st);
            }
            Op::StrictEqAtom {
                eq,
                a,
                atom_bits,
                atom_ok,
            } => {
                let ra = self.gpr(a, RDX);
                let rok = self.gpr(atom_ok, R11);
                let (d, st) = self.dst_gpr(v);
                let cold = self.ops.new_dynamic_label();
                let is_eq = self.ops.new_dynamic_label();
                let done = self.ops.new_dynamic_label();
                dynasm!(self.ops
                    ; mov rcx, QWORD atom_bits as i64
                    ; cmp Rq(ra), rcx
                    ; je => is_eq
                    ; test Rd(rok), Rd(rok)
                    ; jz => cold
                    ; mov Rd(d), i32::from(!eq)
                    ; jmp => done
                    ; => is_eq
                    ; mov Rd(d), i32::from(eq)
                    ; => done
                );
                self.deferred_cold.push(ColdTask::StrictEqAtom {
                    lbl: cold,
                    a,
                    eq,
                    dst: Some((d, done)),
                    branch: None,
                });
                self.done_gpr(d, st);
            }
            Op::BitNotI(a) => {
                let ra = self.gpr(a, RDX);
                let (d, st) = self.dst_gpr(v);
                dynasm!(self.ops
                    ; mov eax, Rd(ra)
                    ; not eax
                    ; mov Rd(d), eax
                );
                self.done_gpr(d, st);
            }
            Op::IsNullish { is_ne, a, ip, inlined } => {
                let s = i.fs.expect("IsNullish has a frame state");
                let bail = if inlined {
                    self.use_stub(fs)
                } else {
                    self.add_plain_bail(s, ip)
                };
                let ra = self.gpr(a, RDX);
                let (d, st) = self.dst_gpr(v);
                let is_null = self.ops.new_dynamic_label();
                let not_null = self.ops.new_dynamic_label();
                let done = self.ops.new_dynamic_label();
                dynasm!(self.ops
                    ; cmp BYTE [rdi + crate::vm::host_api::JIT_HTMLDDA_SCALAR_ENABLED_OFFSET as i32], 0
                    ; je => bail
                    ; mov rax, Rq(ra)
                    ; mov rcx, QWORD Value::UNDEFINED.bits() as i64
                    ; cmp rax, rcx
                    ; je => is_null
                    ; mov rcx, QWORD Value::NULL.bits() as i64
                    ; cmp rax, rcx
                    ; je => is_null
                    ; mov rcx, rax
                    ; shr rcx, 48
                    ; cmp ecx, 0x7FFD
                    ; jne => not_null
                    ; cmp eax, DWORD [rdi + crate::vm::host_api::JIT_HTMLDDA_IDX_OFFSET as i32]
                    ; je => is_null
                    ; => not_null
                    ; mov Rd(d), i32::from(is_ne)
                    ; jmp => done
                    ; => is_null
                    ; mov Rd(d), i32::from(!is_ne)
                    ; => done
                );
                self.done_gpr(d, st);
            }
            Op::LooseEq { is_ne, a, b, ip, inlined } => {
                let s = i.fs.expect("LooseEq has a frame state");
                let bail = if inlined {
                    self.use_stub(fs)
                } else {
                    self.add_plain_bail(s, ip)
                };
                let lstub = self.shared.loose_eq as i64;
                let ra = self.gpr(a, RDX);
                dynasm!(self.ops ; mov rax, Rq(ra));
                let rb = self.gpr(b, R11);
                dynasm!(self.ops
                    ; mov r11, Rq(rb)
                    ; mov rdx, rax
                    ; mov r8, r11
                    ; mov r9d, i32::from(is_ne)
                    ; mov r10, QWORD lstub
                    ; call r10
                    ; mov r10, QWORD crate::codegen::SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                );
                let (d, st) = self.dst_gpr(v);
                dynasm!(self.ops
                    ; and eax, 1
                    ; mov Rd(d), eax
                );
                self.done_gpr(d, st);
            }
            Op::IsArray { val, callee, this_v, ip, inlined } => {
                let s = i.fs.expect("IsArray has a frame state");
                let bail = if inlined {
                    self.use_stub(fs)
                } else {
                    self.add_plain_bail(s, ip)
                };
                let astub = self.shared.is_array as i64;
                let rv = self.gpr(val, RDX);
                dynasm!(self.ops ; mov rax, Rq(rv));
                let rc = self.gpr(callee, R10);
                dynasm!(self.ops ; mov r10, Rq(rc));
                let rt = self.gpr(this_v, R11);
                dynasm!(self.ops
                    ; mov r11, Rq(rt)
                    ; mov rdx, rax
                    ; mov r8, r10
                    ; mov r9, r11
                    ; mov r10, QWORD astub
                    ; call r10
                    ; mov r10, QWORD crate::codegen::SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                );
                let (d, st) = self.dst_gpr(v);
                dynasm!(self.ops
                    ; and eax, 1
                    ; mov Rd(d), eax
                );
                self.done_gpr(d, st);
            }
            Op::InstanceOf { val, ctor, ip, packed } => {
                let s = i.fs.expect("InstanceOf has a frame state");
                let (d, st) = self.dst_gpr(v);
                #[cfg(not(feature = "safe-sandbox"))]
                if crate::vm::instanceof_memo_enabled() {
                    let stub = self.shared.instanceof_pair as i64;
                    let miss = self.ops.new_dynamic_label();
                    let done = self.ops.new_dynamic_label();
                    let rv = self.gpr(val, RDX);
                    dynasm!(self.ops ; mov rax, Rq(rv));
                    let rc = self.gpr(ctor, R11);
                    dynasm!(self.ops
                        ; mov r11, Rq(rc)
                        ; mov rdx, rax
                        ; mov r8, r11
                        ; mov r10, QWORD stub
                        ; call r10
                        ; mov r10, QWORD crate::codegen::SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; je => miss
                        ; mov Rq(d), rax
                        ; => done
                    );
                    self.deferred_cold.push(ColdTask::InstanceOfSlow {
                        lbl: miss,
                        done,
                        d,
                        s,
                        ip,
                        packed,
                    });
                    self.invalidate_window_state(s);
                    self.done_gpr(d, st);
                    return;
                }
                self.call_out(s, crate::vm::jit_instanceof_ic as *const () as u64, ip, packed, 0);
                dynasm!(self.ops ; mov Rq(d), rax);
                self.done_gpr(d, st);
            }
            Op::TypeOf(a) => {
                let s = i.fs.expect("TypeOf has a frame state");
                let heap = f.heap.expect("Tier O bodies have their helpers");
                self.write_state(s);
                let ra = self.gpr(a, RDX);
                dynasm!(self.ops
                    ; mov rdx, Rq(ra)
                    ; mov rcx, rdi
                    ; mov rax, QWORD heap.typeof_str as i64
                    ; call rax
                );
                let (d, st) = self.dst_gpr(v);
                dynasm!(self.ops ; mov Rq(d), rax);
                self.done_gpr(d, st);
            }
            Op::TypeOfIs { a, code, neg } => {
                let ra = self.gpr(a, RDX);
                let (d, st) = self.dst_gpr(v);
                let lit = crate::bytecode::TYPEOF_NAMES.get(code as usize).copied();
                let ans = |name: &str| -> i32 { i32::from((lit == Some(name)) != neg) };
                let (r_num, r_str, r_bool, r_undef, r_obj, r_fn) = (
                    ans("number"),
                    ans("string"),
                    ans("boolean"),
                    ans("undefined"),
                    ans("object"),
                    ans("function"),
                );
                let code_neg = (code as u32 | ((neg as u32) << 8)) as i32;
                let (
                    is_num,
                    is_bool,
                    is_undef,
                    is_obj,
                    is_str,
                    is_str_check,
                    is_fn,
                    heap_val,
                    heap_non_dict,
                    helper_call,
                    done,
                ) = (
                    self.ops.new_dynamic_label(),
                    self.ops.new_dynamic_label(),
                    self.ops.new_dynamic_label(),
                    self.ops.new_dynamic_label(),
                    self.ops.new_dynamic_label(),
                    self.ops.new_dynamic_label(),
                    self.ops.new_dynamic_label(),
                    self.ops.new_dynamic_label(),
                    self.ops.new_dynamic_label(),
                    self.ops.new_dynamic_label(),
                    self.ops.new_dynamic_label(),
                );
                let hot_off = crate::vm::host_api::JIT_HOT_MIRROR_RAW_OFFSET as i32;
                let hot_fid = crate::vm::host_api::JIT_HOT_FID_OFF as i32;
                let special_max = crate::vm::host_api::JIT_TYPEOF_SPECIAL_MAX_OFFSET as i32;
                dynasm!(self.ops
                    ; mov rax, Rq(ra)
                    ; mov rdx, rax
                    ; shr rdx, 48
                    ; sub edx, 0x7FF9
                    ; cmp edx, 4
                    ; ja => is_num
                    ; je => heap_val
                    ; test edx, edx
                    ; jz => is_num
                    ; cmp edx, 1
                    ; je => is_bool
                    ; cmp edx, 3
                    ; je => is_undef
                    ; mov rcx, QWORD TAG_NULL as i64
                    ; cmp rax, rcx
                    ; je => is_obj
                    ; jmp => helper_call
                    ; => heap_val
                    ; mov ecx, eax
                    ; mov r10, [rdi + hot_off]
                    ; lea r10, [r10 + rcx * 8]
                    ; mov edx, DWORD [r10 + rcx * 8]
                    ; mov r11d, DWORD [r10 + rcx * 8 + hot_fid]
                    ; test edx, edx
                    ; jnz => heap_non_dict
                    ; cmp r11d, crate::heap::FID_MIRROR_STR as i32
                    ; ja => helper_call
                    ; je => is_str_check
                    ; cmp ecx, DWORD [rdi + special_max]
                    ; jbe => helper_call
                    ; cmp r11d, crate::heap::FID_MIRROR_FUNC_MAX as i32
                    ; jae => is_obj
                    ; jmp => is_fn
                    ; => is_str_check
                    ; cmp DWORD [rdi + special_max], -1
                    ; je => helper_call
                    ; jmp => is_str
                    ; => heap_non_dict
                    ; test edx, edx
                    ; js => helper_call
                    ; test r11d, r11d
                    ; jns => helper_call
                    ; cmp ecx, DWORD [rdi + special_max]
                    ; jbe => helper_call
                    ; jmp => is_obj
                    ; => is_num
                    ; mov Rd(d), r_num
                    ; jmp => done
                    ; => is_bool
                    ; mov Rd(d), r_bool
                    ; jmp => done
                    ; => is_undef
                    ; mov Rd(d), r_undef
                    ; jmp => done
                    ; => is_obj
                    ; mov Rd(d), r_obj
                    ; jmp => done
                    ; => is_str
                    ; mov Rd(d), r_str
                    ; jmp => done
                    ; => is_fn
                    ; mov Rd(d), r_fn
                    ; => done
                );
                self.deferred_cold.push(ColdTask::TypeOfIs {
                    lbl: helper_call,
                    code_neg,
                    d,
                    done,
                });
                self.done_gpr(d, st);
            }
            Op::TypeOfSame { a, b, neg } => {
                let heap = f.heap.expect("Tier O bodies have their helpers");
                let ra = self.gpr(a, RDX);
                dynasm!(self.ops ; mov rax, Rq(ra));
                let rb = self.gpr(b, R11);
                dynasm!(self.ops
                    ; mov r11, Rq(rb)
                    ; mov rcx, rdi
                    ; mov rdx, rax
                    ; mov r8, r11
                    ; mov r9d, i32::from(neg)
                    ; mov rax, QWORD heap.typeof_same as i64
                    ; call rax
                );
                let (d, st) = self.dst_gpr(v);
                dynasm!(self.ops
                    ; and eax, 1
                    ; mov Rd(d), eax
                );
                self.done_gpr(d, st);
            }
            Op::LoadGlobalOrUndef(idx) => {
                let stub = self.use_stub(fs);
                let (d, st) = self.dst_gpr(v);
                let base = crate::vm::host_api::JIT_GLOBALS_RAW_OFFSET as i32;
                let off = 8 * idx as i32;
                dynasm!(self.ops
                    ; cmp BYTE [rdi + crate::vm::host_api::JIT_ROUTE_DIRTY_OFFSET as i32], 0
                    ; jne => stub
                    ; mov rax, [rdi + base]
                    ; mov Rq(d), [rax + off]
                    ; mov rax, QWORD Value::UNINITIALIZED.bits() as i64
                    ; mov rcx, QWORD Value::UNDEFINED.bits() as i64
                    ; cmp Rq(d), rax
                    ; cmove Rq(d), rcx
                );
                self.done_gpr(d, st);
            }
            Op::LoadCallee { ip } => {
                let s = i.fs.expect("LoadCallee has a frame state");
                let bail = self.add_plain_bail(s, ip);
                dynasm!(self.ops
                    ; mov rcx, rdi
                    ; mov rax, QWORD crate::vm::jit_load_callee as *const () as i64
                    ; call rax
                    ; mov r10, QWORD crate::codegen::SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                );
                let (d, st) = self.dst_gpr(v);
                dynasm!(self.ops ; mov Rq(d), rax);
                self.done_gpr(d, st);
            }
            Op::TailCallGuard { ip } => {
                let s = i.fs.expect("TailCallGuard has a frame state");
                let bail = self.add_plain_bail(s, ip);
                let depth_off = crate::vm::host_api::JIT_CALL_DEPTH_OFFSET as i32;
                dynasm!(self.ops
                    ; mov eax, [rdi + depth_off]
                    ; cmp eax, crate::vm::native_call_cap() as i32
                    ; jae => bail
                );
            }
            Op::CheckCoercible { src, ip, inlined } => {
                let s = i.fs.expect("CheckCoercible has a frame state");
                let bail = if inlined {
                    self.use_stub(fs)
                } else {
                    self.add_plain_bail(s, ip)
                };
                let rx = self.gpr(src, RDX);
                dynasm!(self.ops
                    ; mov rax, QWORD Value::NULL.bits() as i64
                    ; cmp Rq(rx), rax
                    ; je => bail
                    ; mov rax, QWORD Value::UNDEFINED.bits() as i64
                    ; cmp Rq(rx), rax
                    ; je => bail
                );
            }
            Op::BailOnly { ip } => {
                let s = i.fs.expect("BailOnly has a frame state");
                let epilogue = self.epilogue;
                self.write_state(s);
                dynasm!(self.ops
                    ; mov DWORD [rsi], ip as i32
                    ; xor eax, eax
                    ; jmp => epilogue
                );
            }
            Op::ConcatChain {
                a,
                b,
                next_leaf,
                hint,
                ip,
            } => {
                let s = i.fs.expect("ConcatChain has a frame state");
                self.write_state(s);
                let ra = self.gpr(a, RDX);
                dynasm!(self.ops ; mov rax, Rq(ra));
                let rb = self.gpr(b, R11);
                dynasm!(self.ops
                    ; mov r11, Rq(rb)
                    ; mov rcx, rdi
                    ; mov rdx, rax
                    ; mov r8, r11
                    ; mov r9d, hint as i32
                );
                if let Some(next_b) = next_leaf {
                    let helper_ready = self.ops.new_dynamic_label();
                    dynasm!(self.ops
                        ; mov rax, QWORD crate::vm::jit_concat_chain_fast as *const () as i64
                        ; mov r10, [rbx + 8 * next_b as i32]
                        ; shr r10, 48
                        ; cmp r10d, 0x7FF9
                        ; jne => helper_ready
                        ; mov rax, QWORD crate::vm::jit_concat_chain_suffix_fast as *const () as i64
                        ; => helper_ready
                    );
                } else {
                    dynasm!(self.ops
                        ; mov rax, QWORD crate::vm::jit_concat_chain_fast as *const () as i64
                    );
                }
                dynasm!(self.ops ; call rax);
                self.leave_on_sentinel(ip);
                let (d, st) = self.dst_gpr(v);
                dynasm!(self.ops ; mov Rq(d), rax);
                self.done_gpr(d, st);
            }
            Op::CellGet { cell, ip } => {
                let s = i.fs.expect("CellGet has a frame state");
                let bail = self.add_plain_bail(s, ip);
                let rc = self.gpr(cell, RDX);
                let (d, st) = self.dst_gpr(v);
                let mirror = crate::vm::host_api::JIT_CELL_MIRROR_RAW_OFFSET as i32;
                dynasm!(self.ops
                    ; mov eax, Rd(rc)
                    ; mov rcx, [rdi + mirror]
                    ; mov Rq(d), [rcx + rax * 8]
                    ; mov rax, QWORD Value::UNINITIALIZED.bits() as i64
                    ; cmp Rq(d), rax
                    ; je => bail
                );
                self.done_gpr(d, st);
            }
            Op::CellSet { cell, src, checked, ip } => {
                let rc = self.gpr(cell, RDX);
                dynasm!(self.ops ; mov rax, Rq(rc));
                let rs = self.gpr(src, R11);
                if checked {
                    let s = i.fs.expect("CellSetChecked has a frame state");
                    let bail = self.add_plain_bail(s, ip);
                    dynasm!(self.ops
                        ; mov r11, Rq(rs)
                        ; mov rcx, rdi
                        ; mov rdx, rax
                        ; mov r8, r11
                        ; mov rax, QWORD crate::vm::jit_cell_set_tdz_checked as *const () as i64
                        ; call rax
                        ; mov r10, QWORD crate::codegen::SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; je => bail
                    );
                } else {
                    let mirror = crate::vm::host_api::JIT_CELL_MIRROR_RAW_OFFSET as i32;
                    let barrier = self.ops.new_dynamic_label();
                    let done = self.ops.new_dynamic_label();
                    dynasm!(self.ops
                        ; mov r11, Rq(rs)
                        ; mov edx, eax
                        ; mov rcx, [rdi + mirror]
                        ; mov [rcx + rdx * 8], r11
                        ; mov rax, r11
                        ; mov rcx, rax
                        ; shr rcx, 48
                        ; cmp ecx, 0x7FFD
                        ; je => barrier
                        ; => done
                    );
                    self.deferred_cold.push(ColdTask::StoreBarrierNoVol {
                        lbl: barrier,
                        done,
                        obj: None,
                    });
                }
            }
            Op::MakeCell {
                reg,
                init,
                tdz,
                fn_name,
                ip,
            } => {
                let s = i.fs.expect("MakeCell has a frame state");
                let ready = if !fn_name {
                    if let Some(iv) = init {
                        self.ensure_window_reg(reg, iv);
                    }
                    super::super::emit_misc::emit_ready_make_cell(&mut self.ops, reg, tdz)
                } else {
                    None
                };
                if let Some(done) = ready {
                    let miss = self.ops.new_dynamic_label();
                    dynasm!(self.ops ; jmp => miss ; => done);
                    self.deferred_cold.push(ColdTask::MakeHelperSlow {
                        lbl: miss,
                        done,
                        helper: crate::vm::jit_make_cell as *const () as u64,
                        s,
                        ip,
                    });
                    self.invalidate_window_state(s);
                } else {
                    self.write_state(s);
                    let fip = ((f.func_id as u64) << 32) | ip as u64;
                    let window = u64::from(f.reg_count.max(1));
                    dynasm!(self.ops
                        ; mov rcx, rdi
                        ; mov rdx, rbx
                        ; mov r8, QWORD fip as i64
                        ; mov r9, QWORD window as i64
                        ; mov rax, QWORD crate::vm::jit_make_cell as *const () as i64
                        ; call rax
                    );
                    self.leave_on_sentinel(ip);
                }
                let (d, st) = self.dst_gpr(v);
                dynasm!(self.ops ; mov Rq(d), [rbx + 8 * reg as i32]);
                self.done_gpr(d, st);
                self.invalidate_window_reg(reg);
            }
            Op::MakeClosure { dst, ip, captures } => {
                let s = i.fs.expect("MakeClosure has a frame state");
                let ready = if let Some(plan) = f.plain_closures.get(&ip) {
                    for &(r, cv) in &captures {
                        self.ensure_window_reg(r, cv);
                    }
                    super::super::emit_misc::emit_ready_make_closure(&mut self.ops, plan, dst)
                } else {
                    None
                };
                if let Some(done) = ready {
                    let miss = self.ops.new_dynamic_label();
                    dynasm!(self.ops ; jmp => miss ; => done);
                    self.deferred_cold.push(ColdTask::MakeHelperSlow {
                        lbl: miss,
                        done,
                        helper: crate::vm::jit_make_closure as *const () as u64,
                        s,
                        ip,
                    });
                    self.invalidate_window_state(s);
                } else {
                    self.write_state(s);
                    let fip = ((f.func_id as u64) << 32) | ip as u64;
                    let window = u64::from(f.reg_count.max(1));
                    dynasm!(self.ops
                        ; mov rcx, rdi
                        ; mov rdx, rbx
                        ; mov r8, QWORD fip as i64
                        ; mov r9, QWORD window as i64
                        ; mov rax, QWORD crate::vm::jit_make_closure as *const () as i64
                        ; call rax
                    );
                    self.leave_on_sentinel(ip);
                }
                let (d, st) = self.dst_gpr(v);
                dynasm!(self.ops ; mov Rq(d), rax);
                self.done_gpr(d, st);
                self.invalidate_window_reg(dst);
            }
            Op::ThisCheck { src, ip } => {
                let s = i.fs.expect("ThisCheck has a frame state");
                let bail = self.add_plain_bail(s, ip);
                let rx = self.gpr(src, RDX);
                let (d, st) = self.dst_gpr(v);
                let tc_slow = self.ops.new_dynamic_label();
                let tc_done = self.ops.new_dynamic_label();
                let tdz_off = crate::vm::host_api::JIT_THIS_TDZ_LEN_OFFSET as i32;
                let st_off = crate::vm::host_api::JIT_SUPER_THIS_NE_OFFSET as i32;
                dynasm!(self.ops
                    ; mov rax, Rq(rx)
                    ; cmp QWORD [rdi + tdz_off], 0
                    ; jne => tc_slow
                    ; cmp BYTE [rdi + st_off], 0
                    ; jne => tc_slow
                    ; mov Rq(d), rax
                    ; => tc_done
                );
                self.deferred_cold.push(ColdTask::ThisCheckSlow {
                    lbl: tc_slow,
                    done: tc_done,
                    d,
                    bail,
                });
                self.done_gpr(d, st);
            }
            Op::SuperCtorFetch {
                home_class_id,
                baked,
                epoch,
            } => {
                let (d, st) = self.dst_gpr(v);
                if let Some((home_idx, parent_bits)) = baked {
                    let scf_done = self.ops.new_dynamic_label();
                    let scf_slow = self.ops.new_dynamic_label();
                    let scf_baked = self.ops.new_dynamic_label();
                    let epoch_off = crate::vm::host_api::JIT_MI_CLASS_EPOCH_OFFSET as i32;
                    let proto_len_off = crate::vm::host_api::JIT_PROTO_LEN_OFFSET as i32;
                    let proto_raw_off = crate::vm::host_api::JIT_PROTO_RAW_OFFSET as i32;
                    dynasm!(self.ops
                        ; cmp DWORD [rdi + epoch_off], epoch as i32
                        ; jne => scf_slow
                        ; mov eax, home_idx as i32
                        ; cmp rax, QWORD [rdi + proto_len_off]
                        ; jae => scf_baked
                        ; mov r11, [rdi + proto_raw_off]
                        ; mov r10, [r11 + rax * 8]
                        ; test r10, r10
                        ; jz => scf_baked
                        ; mov Rq(d), r10
                        ; jmp => scf_done
                        ; => scf_baked
                        ; mov Rq(d), QWORD parent_bits as i64
                        ; => scf_done
                    );
                    self.deferred_cold.push(ColdTask::SuperCtorFetchSlow {
                        lbl: scf_slow,
                        done: scf_done,
                        d,
                        home_class_id,
                    });
                } else {
                    dynasm!(self.ops
                        ; mov rcx, rdi
                        ; mov edx, home_class_id as i32
                        ; mov rax, QWORD crate::vm::jit_super_ctor_fetch as *const () as i64
                        ; call rax
                        ; mov Rq(d), rax
                    );
                }
                self.done_gpr(d, st);
            }
            Op::ToPropKey { obj, src, ip } => {
                let s = i.fs.expect("ToPropKey has a frame state");
                let ro = self.gpr(obj, RDX);
                let rs = self.gpr(src, R11);
                let (d, st) = self.dst_gpr(v);
                let tpk_slow = self.ops.new_dynamic_label();
                let tpk_done = self.ops.new_dynamic_label();
                dynasm!(self.ops
                    ; mov rax, Rq(ro)
                    ; shr rax, 48
                    ; cmp eax, 0x7FFB
                    ; je => tpk_slow
                    ; cmp eax, 0x7FFC
                    ; je => tpk_slow
                    ; mov rax, Rq(rs)
                    ; mov r10, rax
                    ; shr r10, 48
                    ; cmp r10d, 0x7FFD
                    ; jne => tpk_done
                );
                #[cfg(not(feature = "safe-sandbox"))]
                {
                    use crate::vm::host_api::{
                        JIT_HOT_FID_OFF, JIT_HOT_MIRROR_LEN_OFFSET, JIT_HOT_MIRROR_RAW_OFFSET,
                    };
                    dynasm!(self.ops
                        ; mov r10d, eax
                        ; cmp r10d, DWORD [rdi + JIT_HOT_MIRROR_LEN_OFFSET as i32]
                        ; jae => tpk_slow
                        ; mov rcx, [rdi + JIT_HOT_MIRROR_RAW_OFFSET as i32]
                        ; shl r10, 4
                        ; cmp DWORD [rcx + r10 + JIT_HOT_FID_OFF as i32], crate::heap::FID_MIRROR_STR as i32
                        ; jne => tpk_slow
                    );
                }
                #[cfg(feature = "safe-sandbox")]
                dynasm!(self.ops ; jmp => tpk_slow);
                dynasm!(self.ops
                    ; => tpk_done
                    ; mov Rq(d), rax
                );
                let tpk_join = self.ops.new_dynamic_label();
                dynasm!(self.ops ; => tpk_join);
                self.deferred_cold.push(ColdTask::ColdOpSlow {
                    lbl: tpk_slow,
                    done: tpk_join,
                    d,
                    s,
                    ip,
                    cold_op: 14,
                });
                self.invalidate_window_state(s);
                self.done_gpr(d, st);
            }
            Op::ToConcatKey { src, ip } => {
                let s = i.fs.expect("ToConcatKey has a frame state");
                let rs = self.gpr(src, RDX);
                let (d, st) = self.dst_gpr(v);
                let tck_slow = self.ops.new_dynamic_label();
                let tck_done = self.ops.new_dynamic_label();
                dynasm!(self.ops
                    ; mov rax, Rq(rs)
                    ; mov r10, rax
                    ; shr r10, 48
                    ; cmp r10d, 0x7FFD
                    ; jne => tck_done
                );
                #[cfg(not(feature = "safe-sandbox"))]
                {
                    use crate::vm::host_api::{
                        JIT_HOT_FID_OFF, JIT_HOT_MIRROR_LEN_OFFSET, JIT_HOT_MIRROR_RAW_OFFSET,
                    };
                    dynasm!(self.ops
                        ; mov r10d, eax
                        ; cmp r10d, DWORD [rdi + JIT_HOT_MIRROR_LEN_OFFSET as i32]
                        ; jae => tck_slow
                        ; mov rcx, [rdi + JIT_HOT_MIRROR_RAW_OFFSET as i32]
                        ; shl r10, 4
                        ; cmp DWORD [rcx + r10 + JIT_HOT_FID_OFF as i32], crate::heap::FID_MIRROR_STR as i32
                        ; jne => tck_slow
                    );
                }
                #[cfg(feature = "safe-sandbox")]
                dynasm!(self.ops ; jmp => tck_slow);
                dynasm!(self.ops
                    ; => tck_done
                    ; mov Rq(d), rax
                );
                let tck_join = self.ops.new_dynamic_label();
                dynasm!(self.ops ; => tck_join);
                self.deferred_cold.push(ColdTask::ColdOpSlow {
                    lbl: tck_slow,
                    done: tck_join,
                    d,
                    s,
                    ip,
                    cold_op: 17,
                });
                self.invalidate_window_state(s);
                self.done_gpr(d, st);
            }
            Op::InitDataPropDyn {
                obj_reg,
                obj,
                key,
                val,
                ip,
            } => {
                let s = i.fs.expect("InitDataPropDyn has a frame state");
                self.write_state(s);
                let ro = self.gpr(obj, RDX);
                dynasm!(self.ops ; mov rax, Rq(ro));
                let rk = self.gpr(key, R10);
                dynasm!(self.ops ; mov r10, Rq(rk));
                let rv = self.gpr(val, R11);
                let idp_done = self.ops.new_dynamic_label();
                dynasm!(self.ops
                    ; mov r9, Rq(rv)
                    ; mov r8, r10
                    ; mov rdx, rax
                    ; mov rcx, rdi
                    ; mov rax, QWORD crate::vm::jit_init_data_prop_dyn as *const () as i64
                    ; call rax
                    ; test rax, rax
                    ; jz => idp_done
                );
                self.call_written(crate::vm::jit_cold_op_ic as *const () as u64, ip, 0, 15);
                dynasm!(self.ops ; => idp_done);
                // Reload `obj` from the window (written by `write_state(s)` or
                // immutable read-only param) so a volatile-register `obj` is
                // never read after the native call clobbered `r8`/`r9`.
                let (d, st) = self.dst_gpr(v);
                dynasm!(self.ops ; mov Rq(d), [rbx + 8 * obj_reg as i32]);
                self.done_gpr(d, st);
            }
            Op::MakeFunc { ip } => {
                let s = i.fs.expect("MakeFunc has a frame state");
                self.write_state(s);
                let fip = ((f.func_id as u64) << 32 | ip as u64) as i64;
                dynasm!(self.ops
                    ; mov rcx, rdi
                    ; mov rdx, QWORD fip
                    ; mov rax, QWORD crate::vm::jit_make_func as *const () as i64
                    ; call rax
                );
                self.leave_on_sentinel(ip);
                let (d, st) = self.dst_gpr(v);
                dynasm!(self.ops ; mov Rq(d), rax);
                self.done_gpr(d, st);
            }
            Op::NewObject { hint, ip } => {
                let s = i.fs.expect("NewObject has a frame state");
                self.write_state(s);
                dynasm!(self.ops
                    ; mov rcx, rdi
                    ; mov edx, hint as i32
                    ; mov rax, QWORD crate::vm::jit_new_object as *const () as i64
                    ; call rax
                );
                self.leave_on_sentinel(ip);
                let (d, st) = self.dst_gpr(v);
                dynasm!(self.ops ; mov Rq(d), rax);
                self.done_gpr(d, st);
            }
            Op::NewPlannedObject { plan, ip } => {
                let s = i.fs.expect("NewPlannedObject has a frame state");
                self.write_state(s);
                let packed = ((f.func_id as u64) << 32 | plan as u64) as i64;
                dynasm!(self.ops
                    ; mov rcx, rdi
                    ; mov rdx, QWORD packed
                    ; mov rax, QWORD crate::vm::jit_new_planned_object as *const () as i64
                    ; call rax
                );
                self.leave_on_sentinel(ip);
                let (d, st) = self.dst_gpr(v);
                dynasm!(self.ops ; mov Rq(d), rax);
                self.done_gpr(d, st);
            }
            Op::AppendDataProp { obj, name, val, ip } => {
                let s = i.fs.expect("AppendDataProp has a frame state");
                self.write_state(s);
                let ro = self.gpr(obj, RDX);
                dynasm!(self.ops ; mov rax, Rq(ro));
                let rv = self.gpr(val, R11);
                let packed = ((f.func_id as u64) << 32 | name as u64) as i64;
                dynasm!(self.ops
                    ; mov r11, Rq(rv)
                    ; mov rcx, rdi
                    ; mov rdx, rax
                    ; mov r8, QWORD packed
                    ; mov r9, r11
                    ; mov rax, QWORD crate::vm::jit_append_data_prop as *const () as i64
                    ; call rax
                );
                self.leave_on_sentinel(ip);
            }
            Op::ObjectKeys { obj, callee, this_v, ip } => {
                let s = i.fs.expect("ObjectKeys has a frame state");
                self.write_state(s);
                let ro = self.gpr(obj, RDX);
                dynasm!(self.ops ; mov rax, Rq(ro));
                let rc = self.gpr(callee, R10);
                dynasm!(self.ops ; mov r10, Rq(rc));
                let rt = self.gpr(this_v, R11);
                dynasm!(self.ops
                    ; mov r11, Rq(rt)
                    ; mov rcx, rdi
                    ; mov rdx, rax
                    ; mov r8, r10
                    ; mov r9, r11
                    ; mov rax, QWORD crate::vm::jit_object_keys as *const () as i64
                    ; call rax
                );
                self.leave_on_sentinel(ip);
                let (d, st) = self.dst_gpr(v);
                dynasm!(self.ops ; mov Rq(d), rax);
                self.done_gpr(d, st);
            }
            Op::HasProp { key, obj, ip, inlined } => {
                let s = i.fs.expect("HasProp has a frame state");
                let bail = if inlined {
                    self.use_stub(fs)
                } else {
                    self.add_plain_bail(s, ip)
                };
                let heap = f.heap.expect("Tier O bodies have their helpers");
                let rk = self.gpr(key, RDX);
                dynasm!(self.ops ; mov rax, Rq(rk));
                let ro = self.gpr(obj, R11);
                dynasm!(self.ops
                    ; mov r11, Rq(ro)
                    ; mov rcx, rdi
                    ; mov rdx, rax
                    ; mov r8, r11
                    ; mov rax, QWORD heap.has_property as i64
                    ; call rax
                    ; mov r10, QWORD crate::codegen::SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                );
                let (d, st) = self.dst_gpr(v);
                dynasm!(self.ops
                    ; and eax, 1
                    ; mov Rd(d), eax
                );
                self.done_gpr(d, st);
            }
            Op::LenOf { obj } => {
                let heap = f.heap.expect("Tier O bodies have their helpers");
                let ro = self.gpr(obj, RDX);
                dynasm!(self.ops
                    ; mov rdx, Rq(ro)
                    ; mov rcx, rdi
                    ; mov rax, QWORD heap.len_of as i64
                    ; call rax
                );
                let (d, st) = self.dst_gpr(v);
                dynasm!(self.ops ; mov Rq(d), rax);
                self.done_gpr(d, st);
            }
            Op::ForInKeys { obj, ip } => {
                let s = i.fs.expect("ForInKeys has a frame state");
                let heap = f.heap.expect("Tier O bodies have their helpers");
                self.write_state(s);
                let ro = self.gpr(obj, RDX);
                dynasm!(self.ops
                    ; mov rdx, Rq(ro)
                    ; mov rcx, rdi
                    ; mov rax, QWORD heap.forin_keys as i64
                    ; call rax
                );
                self.leave_on_sentinel(ip);
                let (d, st) = self.dst_gpr(v);
                dynasm!(self.ops ; mov Rq(d), rax);
                self.done_gpr(d, st);
            }
            Op::ForInLive { obj, key } => {
                let s = i.fs.expect("ForInLive has a frame state");
                let heap = f.heap.expect("Tier O bodies have their helpers");
                self.write_state(s);
                let ro = self.gpr(obj, RDX);
                dynasm!(self.ops ; mov rax, Rq(ro));
                let rk = self.gpr(key, R11);
                dynasm!(self.ops
                    ; mov r11, Rq(rk)
                    ; mov rcx, rdi
                    ; mov rdx, rax
                    ; mov r8, r11
                    ; mov rax, QWORD heap.forin_live as i64
                    ; call rax
                );
                let (d, st) = self.dst_gpr(v);
                dynasm!(self.ops ; mov Rq(d), rax);
                self.done_gpr(d, st);
            }
            Op::RegExpLitProbe { pattern, flags, ip } => {
                let s = i.fs.expect("RegExpLitProbe has a frame state");
                let bail = self.add_plain_bail(s, ip);
                dynasm!(self.ops
                    ; mov rcx, rdi
                    ; mov edx, f.func_id as i32
                    ; mov r8d, pattern as i32
                    ; mov r9d, flags as i32
                    ; mov rax, QWORD crate::vm::jit_regexp_lit_probe as *const () as i64
                    ; call rax
                    ; mov r10, QWORD crate::codegen::SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                );
                let (d, st) = self.dst_gpr(v);
                dynasm!(self.ops ; mov Rq(d), rax);
                self.done_gpr(d, st);
            }
            Op::RegExpLitTest {
                arg,
                pattern,
                flags,
                ip,
                packed,
            } => {
                let s = i.fs.expect("RegExpLitTest has a frame state");
                self.write_state(s);
                let slow = self.ops.new_dynamic_label();
                let done = self.ops.new_dynamic_label();
                let consts = (pattern as u64 | (flags as u64) << 32) as i64;
                dynasm!(self.ops
                    ; mov rcx, rdi
                    ; mov edx, f.func_id as i32
                    ; mov r8, QWORD consts
                    ; mov r9, [rbx + 8 * arg as i32]
                    ; mov rax, QWORD crate::vm::jit_regexp_lit_test as *const () as i64
                    ; call rax
                    ; mov r10, QWORD crate::codegen::SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => slow
                );
                self.leave_on_sentinel(ip);
                dynasm!(self.ops ; jmp => done ; => slow);
                self.call_written(crate::vm::jit_regexp_lit_test_ic as *const () as u64, ip, packed, 0);
                let (d, st) = self.dst_gpr(v);
                dynasm!(self.ops
                    ; => done
                    ; mov Rq(d), rax
                );
                self.done_gpr(d, st);
            }
            Op::RegExpMethod {
                op,
                callee,
                this_v,
                arg_base,
                argc,
                ip,
                packed,
            } => {
                use crate::bytecode::RegExpMethod as R;
                let s = i.fs.expect("RegExpMethod has a frame state");
                let heap = f.heap.expect("Tier O bodies have their helpers");
                self.write_state(s);
                let done = self.ops.new_dynamic_label();
                let direct_on = match op {
                    R::Test | R::Exec => crate::vm::regexp_call_direct_enabled(),
                    R::MatchAll | R::Match if argc >= 1 => crate::vm::string_regexp_call_direct_enabled(),
                    R::Replace if argc >= 2 => crate::vm::string_regexp_call_direct_enabled(),
                    _ => false,
                };
                if direct_on {
                    let slow = self.ops.new_dynamic_label();
                    dynasm!(self.ops
                        ; mov rcx, rdi
                        ; mov rdx, [rbx + 8 * this_v as i32]
                    );
                    match op {
                        R::Test | R::Exec => {
                            if argc == 0 {
                                dynasm!(self.ops ; mov r8, QWORD Value::UNDEFINED.bits() as i64);
                            } else {
                                dynasm!(self.ops ; mov r8, [rbx + 8 * arg_base as i32]);
                            }
                            dynasm!(self.ops
                                ; mov r9d, i32::from(op == R::Test)
                                ; mov rax, QWORD heap.regexp_call_direct as i64
                            );
                        }
                        R::MatchAll | R::Replace | R::Match => {
                            let op_code = match op {
                                R::Replace => 1,
                                R::Match => 2,
                                _ => 0,
                            };
                            dynasm!(self.ops
                                ; lea r8, [rbx + 8 * arg_base as i32]
                                ; mov r9d, op_code
                                ; mov rax, QWORD heap.string_regexp_call_direct as i64
                            );
                        }
                    }
                    dynasm!(self.ops
                        ; mov r10, [rbx + 8 * callee as i32]
                        ; mov [rsp + 32], r10
                        ; call rax
                        ; mov r10, QWORD crate::codegen::SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; je => slow
                    );
                    self.leave_on_sentinel(ip);
                    dynasm!(self.ops ; jmp => done ; => slow);
                }
                self.call_written(crate::vm::jit_call_with_this_ic as *const () as u64, ip, packed, argc);
                let (d, st) = self.dst_gpr(v);
                dynasm!(self.ops
                    ; => done
                    ; mov Rq(d), rax
                );
                self.done_gpr(d, st);
            }
            Op::IterProbe => {
                dynasm!(self.ops
                    ; mov rcx, rdi
                    ; mov rax, QWORD crate::vm::jit_iter_probe as *const () as i64
                    ; call rax
                );
                let (d, st) = self.dst_gpr(v);
                dynasm!(self.ops ; mov Rq(d), rax);
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
            (Loc::Param(r), Loc::Gpr(b)) => dynasm!(self.ops ; mov Rq(b), [rbx + 8 * r as i32]),
            (Loc::Param(r), Loc::Xmm(b)) => dynasm!(self.ops ; movsd Rx(b), QWORD [rbx + 8 * r as i32]),
            (Loc::Param(r), Loc::Slot(u)) => {
                let p = self.slot(u);
                dynasm!(self.ops ; mov rcx, [rbx + 8 * r as i32] ; mov [rsp + p], rcx);
            }
            (_, Loc::Param(_)) => unreachable!("Loc::Param is read-only"),
            (Loc::Imm, _) | (_, Loc::Imm) => unreachable!("Loc::Imm is materialized directly in edge_moves"),
        }
    }

    fn emit_imm_to(&mut self, v: V, d: Loc) {
        match (self.f.insts[v as usize].op.clone(), d) {
            (Op::ConstI32(c), Loc::Gpr(r)) => dynasm!(self.ops ; mov Rd(r), c),
            (Op::ConstBool(b), Loc::Gpr(r)) => dynasm!(self.ops ; mov Rd(r), i32::from(b)),
            (Op::ConstTagged(bits), Loc::Gpr(r)) => dynasm!(self.ops ; mov Rq(r), QWORD bits as i64),
            (Op::ConstI32(c), Loc::Slot(t)) => {
                let o = self.slot(t);
                dynasm!(self.ops ; mov eax, c ; mov [rsp + o], rax);
            }
            (Op::ConstBool(b), Loc::Slot(t)) => {
                let o = self.slot(t);
                dynasm!(self.ops ; mov eax, i32::from(b) ; mov [rsp + o], rax);
            }
            (Op::ConstTagged(bits), Loc::Slot(t)) => {
                let o = self.slot(t);
                dynasm!(self.ops ; mov rax, QWORD bits as i64 ; mov [rsp + o], rax);
            }
            _ => unreachable!("unexpected immediate edge move to {d:?}"),
        }
    }

    /// The phi moves of the edge `from` -> `to`, as one parallel move.
    fn edge_moves(&mut self, from: usize, to: usize) {
        let tb = &self.f.blocks[to];
        let Some(k) = tb.preds.iter().position(|&p| p as usize == from) else {
            return;
        };
        let mut pending: Vec<(Loc, Loc)> = Vec::new();
        let mut imm_moves: Vec<(V, Loc)> = Vec::new();
        for &phi in &tb.insts {
            if let Op::Phi(ins) = &self.f.insts[phi as usize].op {
                let src_v = ins[k];
                let (s, d) = (self.a.loc[src_v as usize], self.a.loc[phi as usize]);
                if s == Loc::Imm {
                    imm_moves.push((src_v, d));
                } else if s != d {
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
        for (src_v, d) in imm_moves {
            self.emit_imm_to(src_v, d);
        }
    }

    fn emit_cold_task(&mut self, task: ColdTask) {
        let f = self.f;
        match task {
            ColdTask::StoreBarrierNoVol { lbl, done, obj } => {
                dynasm!(self.ops ; => lbl);
                if let Some(obj) = obj {
                    let ro = self.gpr(obj, 1);
                    dynasm!(self.ops ; mov edx, Rd(ro));
                }
                self.emit_store_barrier_no_vol(done);
            }
            ColdTask::CharCodeAtSlow {
                lbl,
                done,
                obj,
                idx,
                d,
                stub,
            } => {
                let heap = f.heap.expect("Tier O bodies have their helpers");
                dynasm!(self.ops ; => lbl);
                self.save_vol();
                let ro = self.gpr(obj, RDX);
                dynasm!(self.ops ; mov rdx, Rq(ro));
                self.key_to_r8(idx);
                dynasm!(self.ops
                    ; mov rcx, rdi
                    ; mov rax, QWORD heap.char_code_at as i64
                    ; call rax
                );
                self.restore_vol();
                dynasm!(self.ops
                    ; mov r10, QWORD crate::codegen::SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => stub
                    ; mov Rq(d), rax
                    ; jmp => done
                );
            }
            ColdTask::StrictEqAtom {
                lbl,
                a,
                eq,
                dst,
                branch,
            } => {
                let stub = self.shared.strict_eq as i64;
                dynasm!(self.ops ; => lbl);
                let ra = self.gpr(a, RDX);
                dynasm!(self.ops ; mov rax, Rq(ra));
                self.save_vol();
                dynasm!(self.ops
                    ; mov r10, QWORD stub
                    ; call r10
                );
                self.restore_vol();
                if let Some((d, done)) = dst {
                    if !eq {
                        dynasm!(self.ops ; xor eax, 1);
                    }
                    dynasm!(self.ops
                        ; mov Rd(d), eax
                        ; jmp => done
                    );
                } else if let Some((eq_lbl, ne_lbl)) = branch {
                    dynasm!(self.ops
                        ; test eax, eax
                        ; jnz => eq_lbl
                        ; jmp => ne_lbl
                    );
                }
            }
            ColdTask::TaggedTruthy {
                lbl,
                a,
                truthy,
                falsy,
            } => {
                let tstub = self.shared.truthy as i64;
                dynasm!(self.ops ; => lbl);
                let ra = self.gpr(a, RDX);
                dynasm!(self.ops
                    ; mov rax, Rq(ra)
                    ; mov r10, rax
                    ; shr r10, 48
                );
                self.save_vol();
                dynasm!(self.ops
                    ; mov r11, QWORD tstub
                    ; call r11
                );
                self.restore_vol();
                dynasm!(self.ops
                    ; test eax, eax
                    ; jz => falsy
                    ; jmp => truthy
                );
            }
            ColdTask::TypeOfIs {
                lbl,
                code_neg,
                d,
                done,
            } => {
                let heap = f.heap.expect("Tier O bodies have their helpers");
                dynasm!(self.ops ; => lbl);
                self.save_vol();
                dynasm!(self.ops
                    ; mov rdx, rax
                    ; mov rcx, rdi
                    ; mov r8d, code_neg
                    ; mov rax, QWORD heap.typeof_is as i64
                    ; call rax
                );
                self.restore_vol();
                dynasm!(self.ops
                    ; and eax, 1
                    ; mov Rd(d), eax
                    ; jmp => done
                );
            }
            ColdTask::GetPropSlow {
                lbl,
                done,
                obj,
                d,
                s,
                ip,
                packed,
            } => {
                let slow_written = self.ops.new_dynamic_label();
                let got = self.ops.new_dynamic_label();
                dynasm!(self.ops ; => lbl);
                self.save_vol();
                let name = (packed >> 32) as u32;
                let key_id = self.prop_key_id(name);
                if let Some(&(ic_site, _)) = self
                    .f
                    .ic_sites
                    .get(&ip)
                    .filter(|_| !crate::vm::icstats_enabled() && key_id != 0)
                {
                    let mega_addr = self.mega_prop_addr(true);
                    let slow_call = self.ops.new_dynamic_label();
                    let ro2 = self.gpr(obj, RDX);
                    let packed_fn = ((f.func_id as u64) << 32) | name as u64;
                    let can_getter = self.can_cross3_dyn();
                    let class_getter = can_getter.then(|| self.ops.new_dynamic_label());
                    dynasm!(self.ops
                        ; mov rdx, Rq(ro2)
                        ; mov r8d, ic_site as i32
                        ; mov r9, QWORD packed_fn as i64
                        ; mov ecx, key_id as i32
                        ; mov r10, QWORD mega_addr
                        ; call r10
                        ; mov r10, QWORD crate::codegen::SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; je => slow_call
                        ; mov r10, QWORD crate::codegen::PROP_VIA_IC as i64
                        ; cmp rax, r10
                        ; je => slow_call
                        ; mov r10, QWORD crate::codegen::PROP_CLASS_GETTER as i64
                        ; cmp rax, r10
                    );
                    if let Some(cg) = class_getter {
                        dynasm!(self.ops ; je => cg);
                    } else {
                        dynasm!(self.ops ; je => slow_call);
                    }
                    dynasm!(self.ops ; jmp => got);
                    if let Some(cg) = class_getter {
                        let dyn_addr = self.shared.cross3_dyn as i64;
                        let threw = self.add_raw_bail(ip);
                        let reg_hi = self.reg_hi();
                        let c3 = self.c3;
                        dynasm!(self.ops
                            ; => cg
                            ; mov [rsp + c3 + 48], r11
                        );
                        self.restore_vol();
                        self.write_state_full(s);
                        let ro3 = self.gpr(obj, RDX);
                        dynasm!(self.ops
                            ; mov rdx, Rq(ro3)
                            ; mov rax, [rsp + c3 + 48]
                            ; lea r8, [rsp + c3 + 48]
                            ; mov ecx, (reg_hi | 0x100)
                            ; mov r10, QWORD dyn_addr
                            ; call r10
                            ; mov r10, QWORD crate::codegen::SELF_CALL_DEOPT as i64
                            ; cmp rax, r10
                            ; je => slow_written
                            ; mov r10, QWORD crate::codegen::CALL_THREW as i64
                            ; cmp rax, r10
                            ; je => threw
                            ; jmp => got
                        );
                    }
                    dynasm!(self.ops ; => slow_call);
                    self.restore_vol();
                }
                self.write_state_full(s);
                dynasm!(self.ops ; => slow_written);
                self.call_written(crate::vm::jit_opt_get_prop_slow as *const () as u64, ip, packed, 0);
                dynasm!(self.ops ; => got);
                self.restore_vol();
                dynasm!(self.ops
                    ; mov Rq(d), rax
                    ; jmp => done
                );
            }
            ColdTask::SetPropSlow {
                lbl,
                done,
                obj,
                val,
                s,
                ip,
                packed,
            } => {
                let ok = self.ops.new_dynamic_label();
                dynasm!(self.ops ; => lbl);
                self.save_vol();
                let name = (packed >> 32) as u32;
                let key_id = self.prop_key_id(name);
                if let Some(&(ic_site, _)) = self
                    .f
                    .ic_sites
                    .get(&ip)
                    .filter(|_| !crate::vm::icstats_enabled() && key_id != 0)
                {
                    let mega_addr = self.mega_set_addr(true);
                    let ro2 = self.gpr(obj, RDX);
                    dynasm!(self.ops ; mov rdx, Rq(ro2));
                    let rv2 = self.gpr(val, R11);
                    let packed_fn = ((f.func_id as u64) << 32) | name as u64;
                    dynasm!(self.ops
                        ; mov r8, Rq(rv2)
                        ; mov r9, QWORD packed_fn as i64
                        ; mov QWORD [rsp + 32], ic_site as i32
                        ; mov ecx, key_id as i32
                        ; mov r10, QWORD mega_addr
                        ; call r10
                        ; test rax, rax
                        ; jz => ok
                    );
                    self.restore_vol();
                }
                self.write_state_full(s);
                self.call_written(crate::vm::jit_set_prop_slow as *const () as u64, ip, packed, 0);
                dynasm!(self.ops ; => ok);
                self.restore_vol();
                dynasm!(self.ops ; jmp => done);
            }
            ColdTask::CallOutPropMiss {
                lbl,
                join,
                obj,
                name,
                s,
                ip,
                packed,
                helper,
                argc,
                ic_miss,
            } => {
                use super::super::PROP_VIA_IC;
                let slow_written = self.ops.new_dynamic_label();
                dynasm!(self.ops ; => lbl);
                if let Some((site, direct)) = ic_miss {
                    let heap = f.heap.expect("ic_miss requires heap helpers");
                    let key_id = self.prop_key_id(name);
                    let mega_addr = (!crate::vm::icstats_enabled() && key_id != 0).then(|| self.mega_prop_addr(direct));
                    let decline = self.ops.new_dynamic_label();
                    let packed_fn = ((f.func_id as u64) << 32) | name as u64;
                    let c3 = self.c3;
                    if let Some(maddr) = mega_addr {
                        dynasm!(self.ops
                            ; mov rdx, rax
                            ; mov r8d, site as i32
                            ; mov r9, QWORD packed_fn as i64
                            ; mov ecx, key_id as i32
                            ; mov r10, QWORD maddr
                            ; call r10
                        );
                    } else {
                        let miss_fn = heap.get_prop_miss;
                        dynasm!(self.ops
                            ; mov rcx, rdi
                            ; mov rdx, rax
                            ; mov r8d, site as i32
                            ; mov r9, QWORD packed_fn as i64
                            ; mov rax, QWORD miss_fn as i64
                            ; call rax
                        );
                    }
                    let can_getter = mega_addr.is_some() && self.can_cross3_dyn();
                    let class_getter = can_getter.then(|| self.ops.new_dynamic_label());
                    dynasm!(self.ops
                        ; mov r10, QWORD crate::codegen::SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; je => decline
                        ; mov r10, QWORD PROP_VIA_IC as i64
                        ; cmp rax, r10
                        ; je => decline
                        ; mov r10, QWORD crate::codegen::PROP_CLASS_GETTER as i64
                        ; cmp rax, r10
                    );
                    if let Some(cg) = class_getter {
                        dynasm!(self.ops ; je => cg);
                    } else {
                        dynasm!(self.ops ; je => decline);
                    }
                    if self.save_ic_r13 {
                        dynasm!(self.ops ; mov r13, [rsp + c3]);
                    }
                    if self.save_ic_r14 {
                        dynasm!(self.ops ; mov r14, [rsp + c3 + 8]);
                    }
                    dynasm!(self.ops ; jmp => join);
                    if let Some(cg) = class_getter {
                        let dyn_addr = self.shared.cross3_dyn as i64;
                        let threw = self.add_raw_bail(ip);
                        let reg_hi = self.reg_hi();
                        dynasm!(self.ops
                            ; => cg
                            ; mov [rsp + c3 + 48], r11
                        );
                        if self.save_ic_r13 {
                            dynasm!(self.ops ; mov r13, [rsp + c3]);
                        }
                        if self.save_ic_r14 {
                            dynasm!(self.ops ; mov r14, [rsp + c3 + 8]);
                        }
                        self.write_state_full(s);
                        dynasm!(self.ops
                            ; mov rax, [rsp + c3 + 48]
                            ; mov rdx, [rbx + 8 * obj as i32]
                            ; lea r8, [rbx + 8 * obj as i32]
                            ; mov ecx, (reg_hi | 0x100)
                            ; mov r10, QWORD dyn_addr
                            ; call r10
                            ; mov r10, QWORD crate::codegen::SELF_CALL_DEOPT as i64
                            ; cmp rax, r10
                            ; je => slow_written
                            ; mov r10, QWORD crate::codegen::CALL_THREW as i64
                            ; cmp rax, r10
                            ; je => threw
                            ; jmp => join
                        );
                    }
                    dynasm!(self.ops ; => decline);
                    if self.save_ic_r13 {
                        dynasm!(self.ops ; mov r13, [rsp + c3]);
                    }
                    if self.save_ic_r14 {
                        dynasm!(self.ops ; mov r14, [rsp + c3 + 8]);
                    }
                }
                self.write_state_full(s);
                dynasm!(self.ops ; => slow_written);
                self.call_written(helper, ip, packed, argc);
                dynasm!(self.ops ; jmp => join);
            }
            ColdTask::CallOutSetPropMiss {
                lbl,
                done,
                val,
                name,
                s,
                ip,
                packed,
                helper,
                argc,
                ic_site,
                ic_direct,
            } => {
                use super::super::PROP_VIA_IC;
                let heap = f.heap.expect("set_prop_ic requires heap helpers");
                let key_id = self.prop_key_id(name);
                let mega_addr = (!crate::vm::icstats_enabled() && key_id != 0).then(|| self.mega_set_addr(ic_direct));
                let decline = self.ops.new_dynamic_label();
                let packed_fn = ((f.func_id as u64) << 32) | name as u64;
                let c3 = self.c3;
                dynasm!(self.ops ; => lbl);
                if let Some(maddr) = mega_addr {
                    dynasm!(self.ops
                        ; mov rdx, rax
                        ; mov r8, [rbx + 8 * val as i32]
                        ; mov r9, QWORD packed_fn as i64
                        ; mov QWORD [rsp + 32], ic_site as i32
                        ; mov ecx, key_id as i32
                        ; mov r10, QWORD maddr
                        ; call r10
                    );
                } else {
                    let miss_fn = heap.set_prop_miss;
                    dynasm!(self.ops
                        ; mov rcx, rdi
                        ; mov rdx, rax
                        ; mov r8, [rbx + 8 * val as i32]
                        ; mov r9, QWORD packed_fn as i64
                        ; mov QWORD [rsp + 32], ic_site as i32
                        ; mov rax, QWORD miss_fn as i64
                        ; call rax
                    );
                }
                dynasm!(self.ops
                    ; mov r10, QWORD crate::codegen::SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => decline
                    ; mov r10, QWORD PROP_VIA_IC as i64
                    ; cmp rax, r10
                    ; je => decline
                );
                if self.save_ic_r13 {
                    dynasm!(self.ops ; mov r13, [rsp + c3]);
                }
                if self.save_ic_r14 {
                    dynasm!(self.ops ; mov r14, [rsp + c3 + 8]);
                }
                dynasm!(self.ops
                    ; jmp => done
                    ; => decline
                );
                if self.save_ic_r13 {
                    dynasm!(self.ops ; mov r13, [rsp + c3]);
                }
                if self.save_ic_r14 {
                    dynasm!(self.ops ; mov r14, [rsp + c3 + 8]);
                }
                self.write_state_full(s);
                self.call_written(helper, ip, packed, argc);
                dynasm!(self.ops ; jmp => done);
            }
            ColdTask::CallOutFallback {
                lbl,
                join,
                write_s,
                helper,
                ip,
                packed,
                argc,
            } => {
                dynasm!(self.ops ; => lbl);
                if let Some(s) = write_s {
                    self.write_state_full(s);
                }
                self.call_written(helper, ip, packed, argc);
                dynasm!(self.ops ; jmp => join);
            }
            ColdTask::LengthSlow {
                lbl,
                done,
                obj,
                d,
                s,
                ip,
                packed,
            } => {
                let heap = f.heap.expect("Tier O bodies have their helpers");
                let got = self.ops.new_dynamic_label();
                dynasm!(self.ops ; => lbl);
                self.save_vol();
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
                self.restore_vol();
                self.write_state_full(s);
                self.call_written(crate::vm::jit_opt_get_prop_slow as *const () as u64, ip, packed, 0);
                dynasm!(self.ops ; => got);
                self.restore_vol();
                dynasm!(self.ops
                    ; mov Rq(d), rax
                    ; jmp => done
                );
            }
            ColdTask::GetIndexSlow {
                lbl,
                done,
                ta_fill,
                obj,
                key,
                d,
                s,
                ip,
                packed,
                inlined,
            } => {
                let full = self.ops.new_dynamic_label();
                let got = self.ops.new_dynamic_label();
                if let Some((fill, rec)) = ta_fill {
                    dynasm!(self.ops ; => fill);
                    self.save_vol();
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
                let kstub = self.shared.keyed_get as i64;
                dynasm!(self.ops ; => lbl);
                self.save_vol();
                let ro = self.gpr(obj, RDX);
                dynasm!(self.ops ; mov rdx, Rq(ro));
                self.key_to_r8(key);
                dynasm!(self.ops
                    ; mov r10, QWORD kstub
                    ; call r10
                    ; mov r10, QWORD crate::codegen::SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; jne => got
                    ; => full
                );
                self.restore_vol();
                if inlined {
                    let deopt_lbl = self.use_stub(Some(s));
                    dynasm!(self.ops ; jmp => deopt_lbl);
                } else {
                    self.write_state_full(s);
                    self.call_written(crate::vm::jit_opt_index_full as *const () as u64, ip, packed, 0);
                }
                dynasm!(self.ops ; => got);
                self.restore_vol();
                dynasm!(self.ops
                    ; mov Rq(d), rax
                    ; jmp => done
                );
            }
            ColdTask::SetIndexSlow {
                lbl,
                done,
                ta_fill,
                obj,
                key,
                val,
                s,
                ip,
                packed,
            } => {
                let full = self.ops.new_dynamic_label();
                let ok = self.ops.new_dynamic_label();
                if let Some((fill, rec)) = ta_fill {
                    dynasm!(self.ops ; => fill);
                    self.save_vol();
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
                        ; jne => ok
                        ; jmp => full
                    );
                }
                dynasm!(self.ops ; => lbl);
                self.save_vol();
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
                    ; jne => ok
                    ; => full
                );
                self.restore_vol();
                self.write_state_full(s);
                self.call_written(crate::vm::jit_opt_index_full as *const () as u64, ip, packed, 0);
                dynasm!(self.ops ; => ok);
                self.restore_vol();
                dynasm!(self.ops ; jmp => done);
            }
            ColdTask::AddTSlow {
                lbl,
                done,
                a,
                b,
                dst,
                s,
                ip,
            } => {
                dynasm!(self.ops ; => lbl);
                self.save_vol();
                self.write_state_full(s);
                let ra = self.gpr(a, RDX);
                dynasm!(self.ops ; mov rax, Rq(ra));
                let rb = self.gpr(b, R11);
                dynasm!(self.ops
                    ; mov r8, Rq(rb)
                    ; mov rdx, rax
                    ; mov rcx, rdi
                    ; mov rax, QWORD crate::vm::jit_concat as *const () as i64
                    ; call rax
                );
                self.leave_on_sentinel(ip);
                self.restore_vol();
                if let Some(d) = dst {
                    dynasm!(self.ops ; mov Rq(d), rax);
                }
                dynasm!(self.ops ; jmp => done);
            }
            ColdTask::UpvalSetSlow { lbl, done, idx, x, s } => {
                let ok = self.ops.new_dynamic_label();
                dynasm!(self.ops ; => lbl);
                self.save_vol();
                self.write_state_full(s);
                let rx = self.gpr(x, R11);
                dynasm!(self.ops
                    ; mov r8, Rq(rx)
                    ; mov edx, idx as i32
                    ; mov rcx, rdi
                    ; mov rax, QWORD crate::vm::jit_tierc_upval_set as *const () as i64
                    ; call rax
                    ; mov r10, QWORD crate::codegen::SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; jne => ok
                );
                self.leave_counted(s);
                dynasm!(self.ops ; => ok);
                self.restore_vol();
                dynasm!(self.ops ; jmp => done);
            }
            ColdTask::ModFSlow { lbl, done, a, b, d } => {
                dynasm!(self.ops ; => lbl);
                self.save_vol();
                let xa = self.xmm(a, X1);
                let xb = self.xmm(b, X2);
                dynasm!(self.ops
                    ; movq rcx, Rx(xa)
                    ; movq rdx, Rx(xb)
                    ; mov rax, QWORD crate::vm::jit_opt_fmod as *const () as i64
                    ; call rax
                );
                self.restore_vol();
                dynasm!(self.ops
                    ; movq Rx(d), rax
                    ; jmp => done
                );
            }
            ColdTask::CmpTSlow {
                lbl,
                done,
                op,
                d,
                stub,
            } => {
                dynasm!(self.ops ; => lbl);
                self.save_vol();
                dynasm!(self.ops
                    ; mov r8, rcx
                    ; mov rdx, rax
                    ; mov rcx, rdi
                    ; mov r9d, op
                    ; mov rax, QWORD crate::vm::jit_str_relational as *const () as i64
                    ; call rax
                );
                self.restore_vol();
                dynasm!(self.ops
                    ; cmp eax, 2
                    ; je => stub
                    ; mov Rd(d), eax
                    ; jmp => done
                );
            }
            ColdTask::InstanceOfSlow {
                lbl,
                done,
                d,
                s,
                ip,
                packed,
            } => {
                dynasm!(self.ops ; => lbl);
                self.write_state_full(s);
                self.call_written(crate::vm::jit_instanceof_ic as *const () as u64, ip, packed, 0);
                dynasm!(self.ops
                    ; mov Rq(d), rax
                    ; jmp => done
                );
            }
            ColdTask::MakeHelperSlow {
                lbl,
                done,
                helper,
                s,
                ip,
            } => {
                let fip = ((f.func_id as u64) << 32) | ip as u64;
                let window = u64::from(f.reg_count.max(1));
                dynasm!(self.ops ; => lbl);
                self.write_state_full(s);
                dynasm!(self.ops
                    ; mov rcx, rdi
                    ; mov rdx, rbx
                    ; mov r8, QWORD fip as i64
                    ; mov r9, QWORD window as i64
                    ; mov rax, QWORD helper as i64
                    ; call rax
                );
                self.leave_on_sentinel(ip);
                dynasm!(self.ops ; jmp => done);
            }
            ColdTask::LiteralSlow {
                lbl,
                join,
                plan_ptr,
                packed,
                ip,
            } => {
                dynasm!(self.ops
                    ; => lbl
                    ; mov rcx, rdi
                    ; mov rdx, rbx
                    ; mov r8, QWORD plan_ptr as i64
                    ; mov r9, QWORD packed as i64
                    ; mov rax, QWORD crate::vm::jit_finalize_object_thin as *const () as i64
                    ; call rax
                );
                self.leave_on_sentinel(ip);
                dynasm!(self.ops ; jmp => join);
            }
            ColdTask::NewLaneSlow {
                lbl,
                join,
                ip,
                packed,
                argc,
            } => {
                dynasm!(self.ops ; => lbl);
                self.call_written(crate::vm::jit_new_ic as *const () as u64, ip, packed, argc);
                dynasm!(self.ops ; jmp => join);
            }
            ColdTask::IterStepSlow {
                lbl,
                done,
                packed,
                idx,
                ip,
            } => {
                let heap = f.heap.expect("Tier O bodies have their helpers");
                dynasm!(self.ops
                    ; => lbl
                    ; mov rcx, rdi
                    ; mov rdx, rbx
                    ; mov r8, QWORD packed as i64
                    ; mov r9d, idx as i32
                    ; mov rax, QWORD heap.iter_next as i64
                    ; call rax
                );
                self.leave_on_sentinel(ip);
                dynasm!(self.ops ; jmp => done);
            }
            ColdTask::ThisCheckSlow { lbl, done, d, bail } => {
                dynasm!(self.ops ; => lbl);
                self.save_vol();
                dynasm!(self.ops
                    ; mov rdx, rax
                    ; mov rcx, rdi
                    ; mov rax, QWORD crate::vm::jit_this_check_ic as *const () as i64
                    ; call rax
                );
                self.restore_vol();
                dynasm!(self.ops
                    ; mov r10, QWORD crate::codegen::SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov Rq(d), rax
                    ; jmp => done
                );
            }
            ColdTask::SuperCtorFetchSlow {
                lbl,
                done,
                d,
                home_class_id,
            } => {
                dynasm!(self.ops ; => lbl);
                self.save_vol();
                dynasm!(self.ops
                    ; mov rcx, rdi
                    ; mov edx, home_class_id as i32
                    ; mov rax, QWORD crate::vm::jit_super_ctor_fetch as *const () as i64
                    ; call rax
                );
                self.restore_vol();
                dynasm!(self.ops
                    ; mov Rq(d), rax
                    ; jmp => done
                );
            }
            ColdTask::ColdOpSlow {
                lbl,
                done,
                d,
                s,
                ip,
                cold_op,
            } => {
                dynasm!(self.ops ; => lbl);
                self.save_vol();
                self.write_state_full(s);
                self.call_written(crate::vm::jit_cold_op_ic as *const () as u64, ip, 0, cold_op);
                self.restore_vol();
                dynasm!(self.ops
                    ; mov Rq(d), rax
                    ; jmp => done
                );
            }
        }
    }
}

pub(crate) fn emit(
    f: &Func,
    a: &Alloc,
    prop_key_ids: &[u32],
    shared: &super::super::proto_mem::SharedStubAddrs,
) -> Option<JitFn> {
    let mut ops = crate::codegen::new_x64_asm();
    let block_labels: Vec<DynamicLabel> = f.blocks.iter().map(|_| ops.new_dynamic_label()).collect();
    let stubs: Vec<DynamicLabel> = f.states.iter().map(|_| ops.new_dynamic_label()).collect();
    let stub_used = vec![false; f.states.len()];
    let epilogue = ops.new_dynamic_label();
    // The callee-saved registers and volatile allocator registers the allocation used.
    let mut gpr_saves: Vec<u8> = Vec::new();
    let mut xmm_saves: Vec<u8> = Vec::new();
    let (mut uses_r8, mut uses_r9, mut uses_x4, mut uses_x5) = (false, false, false, false);
    for l in &a.loc {
        match *l {
            Loc::Gpr(8) => uses_r8 = true,
            Loc::Gpr(9) => uses_r9 = true,
            Loc::Xmm(4) => uses_x4 = true,
            Loc::Xmm(5) => uses_x5 = true,
            Loc::Gpr(r) if [5, 12, 13, 14, 15].contains(&r) && !gpr_saves.contains(&r) => gpr_saves.push(r),
            Loc::Xmm(r) if r >= 6 && !xmm_saves.contains(&r) => xmm_saves.push(r),
            _ => {}
        }
    }
    let has_ic_probe = !f.ic_sites.is_empty()
        && f.insts.iter().any(|inst| {
            matches!(
                inst.op,
                Op::CallOut {
                    lane: CallLane::Prop { .. } | CallLane::SetProp { .. },
                    ..
                }
            )
        });
    let save_ic_r13 = if has_ic_probe && !gpr_saves.contains(&13) {
        gpr_saves.push(13);
        false
    } else {
        true
    };
    let save_ic_r14 = if has_ic_probe && !gpr_saves.contains(&14) {
        gpr_saves.push(14);
        false
    } else {
        true
    };
    gpr_saves.sort_unstable();
    xmm_saves.sort_unstable();
    let pushes = 3 + gpr_saves.len() as i32;
    let xmm_area = 16 * xmm_saves.len() as i32;
    let vol_save = OUT_AREA + xmm_area;
    let vol_area = if uses_r8 || uses_r9 || uses_x4 || uses_x5 {
        VOL_SAVE_AREA
    } else {
        0
    };
    let slot_base = vol_save + vol_area;
    let ta_base = slot_base + 8 * a.slots as i32;
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
    let stress = super::stress_every() != 0;
    // Each value's readers, to fuse a compare into the branch that is its
    // only reader (no boolean made, then tested).
    let mut uses = vec![0u32; f.insts.len()];
    for inst in &f.insts {
        inst.op.for_each_operand(|u| uses[u as usize] += 1);
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
            Term::Switch { val, .. } => uses[val as usize] += 1,
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
            && (!stress || f.insts[cond as usize].fs.is_none())
            && matches!(
                f.insts[cond as usize].op,
                Op::CmpF(..)
                    | Op::CmpI(..)
                    | Op::CmpB(..)
                    | Op::StrictEqBits(..)
                    | Op::StrictEqAtom { .. }
                    | Op::IsNullish { .. }
            ))
        .then_some(cond)
    };
    let mut em = Em {
        ops,
        f,
        a,
        prop_key_ids,
        slot_base,
        stubs,
        stub_used,
        stress,
        epilogue,
        ta_base,
        c3,
        shared,
        plain_bails: Vec::new(),
        raw_bails: Vec::new(),
        pending_jtabs: Vec::new(),
        deferred_cold: Vec::new(),
        save_ic_r13,
        save_ic_r14,
        window_known: Vec::new(),
        vol_save,
        uses_r8,
        uses_r9,
        uses_x4,
        uses_x5,
    };
    for (bi, b) in f.blocks.iter().enumerate() {
        em.window_known.clear();
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
                Op::CmpB(eq, x, y) => {
                    let ra = em.gpr(x, RDX);
                    let rb = em.gpr(y, R11);
                    dynasm!(em.ops ; cmp Rd(ra), Rd(rb));
                    if eq {
                        dynasm!(em.ops ; jne => lf);
                    } else {
                        dynasm!(em.ops ; je => lf);
                    }
                }
                Op::StrictEqBits(eq, a, bits) => {
                    let ra = em.gpr(a, RDX);
                    dynasm!(em.ops
                        ; mov rcx, QWORD bits as i64
                        ; cmp Rq(ra), rcx
                    );
                    if eq {
                        dynasm!(em.ops ; jne => lf);
                    } else {
                        dynasm!(em.ops ; je => lf);
                    }
                }
                Op::StrictEqAtom {
                    eq,
                    a,
                    atom_bits,
                    atom_ok,
                } => {
                    let ra = em.gpr(a, RDX);
                    let rok = em.gpr(atom_ok, R11);
                    let cold = em.ops.new_dynamic_label();
                    let (eq_lbl, ne_lbl) = if eq { (lt, lf) } else { (lf, lt) };
                    dynasm!(em.ops
                        ; mov rcx, QWORD atom_bits as i64
                        ; cmp Rq(ra), rcx
                        ; je => eq_lbl
                        ; test Rd(rok), Rd(rok)
                        ; jz => cold
                    );
                    if ne_lbl != lt {
                        dynasm!(em.ops ; jmp => ne_lbl);
                    }
                    em.deferred_cold.push(ColdTask::StrictEqAtom {
                        lbl: cold,
                        a,
                        eq,
                        dst: None,
                        branch: Some((eq_lbl, ne_lbl)),
                    });
                }
                Op::IsNullish { is_ne, a, ip, inlined } => {
                    let s = f.insts[c as usize].fs.expect("IsNullish has a frame state");
                    let bail = if inlined {
                        em.use_stub(Some(s))
                    } else {
                        em.add_plain_bail(s, ip)
                    };
                    let ra = em.gpr(a, RDX);
                    let (null_lbl, non_null_lbl) = if is_ne { (lf, lt) } else { (lt, lf) };
                    dynasm!(em.ops
                        ; cmp BYTE [rdi + crate::vm::host_api::JIT_HTMLDDA_SCALAR_ENABLED_OFFSET as i32], 0
                        ; je => bail
                        ; mov rax, Rq(ra)
                        ; mov rcx, QWORD Value::UNDEFINED.bits() as i64
                        ; cmp rax, rcx
                        ; je => null_lbl
                        ; mov rcx, QWORD Value::NULL.bits() as i64
                        ; cmp rax, rcx
                        ; je => null_lbl
                        ; mov rcx, rax
                        ; shr rcx, 48
                        ; cmp ecx, 0x7FFD
                        ; jne => non_null_lbl
                        ; cmp eax, DWORD [rdi + crate::vm::host_api::JIT_HTMLDDA_IDX_OFFSET as i32]
                        ; je => null_lbl
                        ; jmp => non_null_lbl
                    );
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
        match &b.term {
            Term::Jump(s) => {
                let s = *s;
                em.edge_moves(bi, s as usize);
                if s as usize != bi + 1 {
                    dynasm!(em.ops ; jmp => block_labels[s as usize]);
                }
            }
            Term::Branch { cond, t, f: fb } => {
                let (cond, t, fb) = (*cond, *t, *fb);
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
            Term::Switch {
                val,
                min_val,
                targets,
                default,
            } => {
                let (val, min_val, default) = (*val, *min_val, *default);
                let mut tramps: Vec<(super::build::B, DynamicLabel)> = Vec::new();
                let mut resolve_tgt = |em: &mut Em, tb: super::build::B| -> DynamicLabel {
                    let has_phi = f.blocks[tb as usize]
                        .insts
                        .iter()
                        .any(|&iv| matches!(f.insts[iv as usize].op, Op::Phi(_)));
                    if !has_phi {
                        return block_labels[tb as usize];
                    }
                    if let Some(&(_, lbl)) = tramps.iter().find(|&&(b_id, _)| b_id == tb) {
                        return lbl;
                    }
                    let lbl = em.ops.new_dynamic_label();
                    tramps.push((tb, lbl));
                    lbl
                };
                let def_lbl = resolve_tgt(&mut em, default);
                let jtab = em.ops.new_dynamic_label();
                let slot_labels: Vec<DynamicLabel> = targets
                    .iter()
                    .map(|opt| match *opt {
                        Some(tb) => resolve_tgt(&mut em, tb),
                        None => def_lbl,
                    })
                    .collect();
                let span = (targets.len() - 1) as i32;
                match em.rep(val) {
                    Rep::I32 => {
                        let rv = em.gpr(val, RDX);
                        dynasm!(em.ops
                            ; mov edx, Rd(rv)
                            ; sub edx, min_val
                            ; cmp edx, span
                            ; ja => def_lbl
                            ; lea r10, [=> jtab]
                            ; lea rax, [r10 + rdx * 8]
                            ; jmp rax
                        );
                        em.pending_jtabs.push((jtab, slot_labels));
                    }
                    Rep::Tagged => {
                        let rv = em.gpr(val, RDX);
                        let zero_lbl = if min_val <= 0 && (min_val as i64 + span as i64) >= 0 {
                            targets[(0 - min_val) as usize].map(|tb| resolve_tgt(&mut em, tb))
                        } else {
                            None
                        };
                        let non_int = zero_lbl.map(|_| em.ops.new_dynamic_label());
                        dynasm!(em.ops
                            ; mov rax, Rq(rv)
                            ; mov r10, rax
                            ; shr r10, 48
                            ; cmp r10d, 0x7FF9
                            ; jne => non_int.unwrap_or(def_lbl)
                            ; mov edx, eax
                            ; sub edx, min_val
                            ; cmp edx, span
                            ; ja => def_lbl
                            ; lea r10, [=> jtab]
                            ; lea rax, [r10 + rdx * 8]
                            ; jmp rax
                        );
                        if let (Some(ni), Some(zl)) = (non_int, zero_lbl) {
                            dynasm!(em.ops
                                ; => ni
                                ; mov r10, QWORD SIGN as i64
                                ; cmp rax, r10
                                ; je => zl
                                ; jmp => def_lbl
                            );
                        }
                        em.pending_jtabs.push((jtab, slot_labels));
                    }
                    Rep::F64 => {
                        let xv = em.xmm(val, X1);
                        dynasm!(em.ops
                            ; cvttsd2si eax, Rx(xv)
                            ; cvtsi2sd xmm0, eax
                            ; ucomisd xmm0, Rx(xv)
                            ; jne => def_lbl
                            ; jp => def_lbl
                            ; mov edx, eax
                            ; sub edx, min_val
                            ; cmp edx, span
                            ; ja => def_lbl
                            ; lea r10, [=> jtab]
                            ; lea rax, [r10 + rdx * 8]
                            ; jmp rax
                        );
                        em.pending_jtabs.push((jtab, slot_labels));
                    }
                    Rep::Bool => {
                        dynasm!(em.ops ; jmp => def_lbl);
                    }
                    Rep::Void | Rep::Raw => unreachable!("switch scrutinee has a JS value rep"),
                }
                for (tb, lbl) in tramps {
                    dynasm!(em.ops ; => lbl);
                    em.edge_moves(bi, tb as usize);
                    dynasm!(em.ops ; jmp => block_labels[tb as usize]);
                }
            }
            Term::Return(v) => {
                let rv = em.gpr(*v, RDX);
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
    // ── out-of-line cold paths ──
    let cold_tasks = std::mem::take(&mut em.deferred_cold);
    for task in cold_tasks {
        em.emit_cold_task(task);
    }
    // ── deopt stubs (emitted only for frame states jumped to by a guard) ──
    let helper = crate::vm::jit_opt_deopt as *const () as i64;
    for (k, s) in f.states.iter().enumerate() {
        if !em.stub_used[k] {
            continue;
        }
        dynasm!(em.ops ; => em.stubs[k]);
        em.write_state_full(k as u32);
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
    // ── non-deopt plain bail stubs ──
    let plain_bails = std::mem::take(&mut em.plain_bails);
    for (lbl, s, ip) in plain_bails {
        dynasm!(em.ops ; => lbl);
        em.write_state_full(s);
        dynasm!(em.ops
            ; mov DWORD [rsi], ip as i32
            ; xor eax, eax
            ; jmp => epilogue
        );
    }
    // ── deduplicated raw bail stubs (frame state already in window) ──
    let raw_bails = std::mem::take(&mut em.raw_bails);
    for (lbl, ip) in raw_bails {
        dynasm!(em.ops
            ; => lbl
            ; mov DWORD [rsi], ip as i32
            ; xor eax, eax
            ; jmp => epilogue
        );
    }
    // ── switch jump tables (8-byte padded entries) ──
    let pending_jtabs = std::mem::take(&mut em.pending_jtabs);
    for (jtab, slot_labels) in pending_jtabs {
        dynasm!(em.ops ; => jtab);
        for tgt in slot_labels {
            let start = em.ops.offset().0;
            dynasm!(em.ops ; jmp => tgt);
            let used = em.ops.offset().0 - start;
            assert!(used <= 8, "Tier O switch jump-table entry exceeded 8-byte stride ({used})");
            for _ in used..8 {
                dynasm!(em.ops ; nop);
            }
        }
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
    let buf = crate::codegen::finalize_code(em.ops)?;
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
