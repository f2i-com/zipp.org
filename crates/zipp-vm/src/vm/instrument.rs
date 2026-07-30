//! Execution instrumentation: step budget, cooperative abort, and an optional
//! per-instruction execution trace.
//!
//! All of this exists for ONE caller shape: a host that runs *untrusted* JS and
//! needs (a) a hard bound on how long a script may run, and (b) a record of what
//! the interpreter actually did, in a form a zero-knowledge prover can turn into
//! polynomial constraints. Neither is wanted by `zipp js file.js`, so the module
//! — and the dispatch-loop hook that reaches it — is behind the `instrument`
//! feature and compiles to nothing in a default build. This engine's inner
//! dispatch loop does not get a branch it cannot use.
//!
//! # Metering keeps the JIT; tracing cannot
//!
//! `try_run_jit` runs a whole function activation natively and `try_run_osr` a
//! whole loop region; both return only a resume-ip, so millions of instructions
//! can execute with zero iterations of the loop this module hooks.
//!
//! For the BUDGET and the ABORT FLAG that is solved rather than avoided:
//! compiled code charges [`super::Vm::jit_steps`] itself, once per basic block,
//! by that block's exact instruction count (`codegen::meter`). The charge is the
//! same number in the same unit the interpreter would have counted — a test
//! pins the two to be equal — so `max_steps` means one thing regardless of how
//! hot the code got. Native code runs on a bounded loan of the budget, which is
//! what makes it hand control back often enough for the abort flag to be
//! polled.
//!
//! For the TRACE there is no such fix, and the JIT genuinely does go off
//! ([`super::Vm::enter_trace_mode`]). A trace has to be a row-per-instruction
//! record; native code produces no rows, so a JIT'd hot loop would simply be
//! missing from it while the program still returned the right answer — and a
//! proof over that trace would attest to an execution that never happened.
//!
//! Two paths execute user work with neither an interpreter loop nor a VM
//! pointer to charge against: the fused array kernels and the off-frame method
//! inliner. The inliner knows its body length and charges it in Rust; the
//! kernels are declined outright in a metered VM (`Vm::jit_fused_ok`).
//!
//! # The trace contract
//!
//! A [`TraceStep`] is deliberately NOT a faithful record of a zipp instruction.
//! It is a row in an algebraic execution table, and each row *claims* an
//! identity that a verifier re-checks over a finite field:
//!
//! | opcode | claim |
//! |---|---|
//! | [`op::CONST`] | `val_dst == const_val` |
//! | [`op::MOVE`], [`op::GET_GLOBAL`] | `val_dst == val_a` |
//! | [`op::SET_GLOBAL`] | `val_dst == 0` |
//! | [`op::ADD`] / [`op::SUB`] / [`op::MUL`] | `val_dst == val_a ∘ val_b` |
//! | [`op::DIV`] | `val_a == val_dst * val_b` |
//! | [`op::MOD`] | `val_a == val_b * aux + val_dst` |
//! | [`op::NEG`] | `val_dst + val_a == 0` |
//! | [`op::NOT`] | `val_dst ∈ {0,1}` |
//! | [`op::CMP`] | `aux ∈ {0,1}` |
//! | [`op::BITWISE`] | `val_dst` equals its own low-8-bit decomposition |
//! | [`op::JUMP`] | the next row's `pc` equals `aux` |
//! | [`op::CALL`] / [`op::RETURN`] | call depth ±1 across the row |
//! | [`op::PROP`] / [`op::COLLECTION`] | call depth unchanged |
//! | [`op::HALT`] | terminal; every later row is also HALT |
//! | [`op::OTHER`] | nothing beyond "a step happened at this clock" |
//!
//! Those identities hold over a prime field, not over JS. `-1`, `2.5` and
//! `"abc"` have no field representation that makes `a + b = c` mean addition. So
//! the recorder is a two-stage, **claim-only-what-checks-out** design: the
//! pre-hook proposes an opcode and samples the operands, and the post-hook (the
//! top of the *next* dispatch iteration, by which point the destination register
//! holds the result) re-checks the identity in `u64` and rewrites the row to
//! [`op::OTHER`] unless it holds exactly. A rewritten row proves less; it never
//! proves something false. That is the only sound way to point a fixed-arity AIR
//! at a dynamically typed language.
//!
//! The numbering below is a WIRE CONTRACT shared with the prover
//! (`zk-zipp`'s `TraceStep::selector_col`). Changing a number here silently
//! moves a row onto a different polynomial constraint. Add at the end.

use crate::bytecode::Instr;
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
use crate::codegen::meter::NATIVE_CHUNK;
use crate::value::Value;

/// Without a JIT tier there is no native code to lend budget to; the constant
/// is only referenced by the metering loan path, which no build without
/// `codegen` can reach. Keep the value in step with `codegen::meter` anyway, so
/// the two never silently disagree about the unit.
#[cfg(not(all(feature = "jit", target_arch = "x86_64")))]
const NATIVE_CHUNK: i64 = 1 << 20;

/// The trace opcode set — one per AIR selector column. A wire contract with the
/// prover; see the module docs.
pub mod op {
    pub const CONST: u8 = 0;
    pub const MOVE: u8 = 1;
    pub const GET_GLOBAL: u8 = 2;
    pub const SET_GLOBAL: u8 = 3;
    pub const ADD: u8 = 4;
    pub const SUB: u8 = 5;
    pub const MUL: u8 = 6;
    pub const DIV: u8 = 7;
    pub const MOD: u8 = 8;
    pub const POW: u8 = 9;
    pub const NEG: u8 = 10;
    pub const NOT: u8 = 11;
    pub const CMP: u8 = 12;
    pub const BITWISE: u8 = 13;
    pub const JUMP: u8 = 14;
    pub const CALL: u8 = 15;
    pub const RETURN: u8 = 16;
    pub const PROP: u8 = 17;
    pub const COLLECTION: u8 = 18;
    pub const HALT: u8 = 19;
    pub const OTHER: u8 = 20;
    /// One past the last opcode — the prover's selector table is this wide.
    pub const COUNT: u8 = 21;
}

/// One row of the algebraic execution table. See the module docs for what each
/// `opcode` obliges the other fields to satisfy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceStep {
    /// Row index. Must be exactly `0, 1, 2, …`: the AIR constrains
    /// `clk[i+1] == clk[i] + 1` on every non-halt row.
    pub clk: u64,
    /// The instruction pointer this step executed. Row 0's must be 0.
    pub pc: u64,
    pub opcode: u8,
    pub val_a: u64,
    pub val_b: u64,
    pub val_dst: u64,
    pub const_val: u64,
    pub aux: u64,
}

/// Why an instrumented run stopped. Surfaced as an uncaught throw so a script
/// cannot `catch` its way past its own budget.
pub(crate) const BUDGET_MSG: &str = "RangeError: script exceeded its instruction budget";
pub(crate) const ABORT_MSG: &str = "RangeError: script execution was aborted by the host";
/// Raised when a script exceeds the heap ceiling its host set.
pub(crate) const MEMORY_MSG: &str = "RangeError: script exceeded its memory budget";

/// A row whose operands have been sampled but whose result has not: the
/// destination register is not written until the instruction runs, so the row is
/// completed at the top of the next dispatch iteration.
struct Pending {
    row: usize,
    /// ABSOLUTE index into the register file (`base + dst`), or `None` for an
    /// instruction with no destination. Absolute because `base` belongs to the
    /// frame that issued the instruction, which may no longer be the top frame
    /// by the time the row is completed (a call pushed onto it).
    dst: Option<usize>,
    /// Frame count before the instruction ran, so the post-hook can tell a call
    /// that pushed a frame from one that dispatched straight to a native.
    frames: usize,
    claim: Claim,
}

/// What the pre-hook proposed. The post-hook either confirms it against the
/// observed result or rewrites the row to [`op::OTHER`].
#[derive(Clone, Copy, PartialEq, Eq)]
enum Claim {
    /// `val_dst == const_val` — both sides are the same encoding, so it holds.
    Const,
    /// `val_dst == val_a`.
    Move,
    GetGlobal,
    SetGlobal,
    /// Integer arithmetic, re-checked exactly in `u64`.
    Arith(u8),
    Neg,
    Not,
    Cmp,
    Bitwise,
    Jump,
    /// Proposed as a call; confirmed only if a frame was actually pushed.
    Call,
    /// Proposed as a return; confirmed only if a frame was actually popped and
    /// the recorded depth is above zero.
    Return,
    /// Depth-constancy only.
    Flat,
    /// No claim at all.
    None,
}

/// How often the abort flag is polled, minus one (checked when
/// `ticks & MASK == 0`). An atomic load per instruction would be a real cost for
/// a flag that is almost never set; 4096 instructions is well under a
/// millisecond, so the host still sees a prompt stop.
const ABORT_CHECK_MASK: u64 = 0xFFF;

/// Per-VM instrumentation state. Allocated only when a host asks for it.
pub(crate) struct Recorder {
    /// Instructions the script may still execute, NOT counting any chunk
    /// currently lent to native code (see `Vm::meter_lend`). Signed because the
    /// native tier charges a whole basic block at a time and can overshoot zero
    /// by up to one block; `i64::MAX` means unlimited.
    pub(crate) remaining: i64,
    /// Instructions actually executed since the recorder was attached: the
    /// interpreter's per-instruction charge, the native tier's net lending
    /// (lent at `meter_lend`, unspent refunded at `meter_return`), and off-loop
    /// work charged through `charge_steps`. The consumed half of `remaining` —
    /// the number a host billing for execution (gas metering) needs, rather
    /// than the cap it enforces.
    pub(crate) used: u64,
    /// Set by another thread to stop a running script.
    pub(crate) abort: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// Approximate heap ceiling in bytes; `usize::MAX` means unlimited.
    ///
    /// Checked on the same schedule as the abort flag, for the same reason:
    /// a script that allocates without bound does so over many instructions,
    /// so noticing a few thousand instructions late costs a bounded overshoot
    /// and keeps the per-instruction path free of an extra memory read.
    pub(crate) heap_limit: usize,
    steps: Vec<TraceStep>,
    /// Whether to record rows at all — metering works without tracing.
    tracing: bool,
    /// Stop recording (but keep executing) past this many rows. A truncated
    /// trace is unprovable, which [`Recorder::finish`] reports as `None`.
    max_steps: usize,
    truncated: bool,
    pending: Option<Pending>,
    /// The call depth the emitted rows imply. The prover recomputes this from
    /// the opcodes, so the two must agree — never emit a RETURN at depth 0.
    depth: u64,
    ticks: u64,
}

impl Recorder {
    pub(crate) fn new() -> Self {
        Self {
            remaining: i64::MAX,
            used: 0,
            abort: None,
            heap_limit: usize::MAX,
            steps: Vec::new(),
            tracing: false,
            max_steps: usize::MAX,
            truncated: false,
            pending: None,
            depth: 0,
            ticks: 0,
        }
    }

    /// Begin recording. `max_steps` bounds memory: at ~64 bytes a row, a hot
    /// loop would otherwise exhaust the host long before it finished.
    pub(crate) fn start_trace(&mut self, max_steps: usize) {
        self.steps.clear();
        self.steps.reserve(max_steps.min(1 << 16));
        self.tracing = true;
        self.max_steps = max_steps;
        self.truncated = false;
        self.pending = None;
        self.depth = 0;
    }

    pub(crate) fn truncated(&self) -> bool {
        self.truncated
    }

    /// Stop recording and hand over the rows, terminated by a [`op::HALT`] row
    /// carrying `result` — the value the prover's boundary assertion pins.
    ///
    /// `None` means the trace is unusable and the caller must fall back to an
    /// unproven receipt: it was truncated, or it is too short to satisfy the
    /// AIR's boundary assertions (row 0 is asserted NOT to be the halt row).
    pub(crate) fn finish(&mut self, result: u64) -> Option<Vec<TraceStep>> {
        self.tracing = false;
        // A pending row's instruction ran but its result was never observed
        // (the program ended, or threw, inside it). Complete it as OTHER: we
        // cannot claim an identity we did not check.
        if let Some(p) = self.pending.take() {
            blank(&mut self.steps[p.row]);
        }
        if self.truncated || self.steps.len() < 2 {
            self.steps.clear();
            return None;
        }
        // pc is otherwise unconstrained (only `pc[0] == 0` and the jump
        // constraint, which reads the FOLLOWING row's pc). Row 0 has no
        // predecessor, so forcing it costs nothing and spares the caller a
        // dependency on where the engine happens to start executing.
        self.steps[0].pc = 0;
        let clk = self.steps.len() as u64;
        let pc = self.steps.last().map(|s| s.pc).unwrap_or(0);
        self.steps.push(TraceStep {
            clk,
            pc,
            opcode: op::HALT,
            val_a: 0,
            val_b: 0,
            val_dst: result,
            const_val: 0,
            aux: 0,
        });
        // A jump's `aux` is normally stamped by the next instruction's pre-hook.
        // When execution stopped ON a jump — budget exhausted, aborted, or a
        // throw — no next instruction ever opened, so the halt row is now its
        // successor and the claim has to point at that.
        let n = self.steps.len();
        if self.steps[n - 2].opcode == op::JUMP {
            self.steps[n - 2].aux = pc;
        }
        Some(std::mem::take(&mut self.steps))
    }
}

/// Strip a row back to a bare "a step happened here" claim.
fn blank(row: &mut TraceStep) {
    row.opcode = op::OTHER;
    row.val_a = 0;
    row.val_b = 0;
    row.val_dst = 0;
    row.const_val = 0;
    row.aux = 0;
}

/// The result of a metering check: `Err` stops the script.
pub(crate) type Tick = Result<(), &'static str>;

impl super::Vm<'_> {
    /// Lend the native tier a slice of the step budget, returning what it took.
    ///
    /// Compiled code charges `Vm::jit_steps` directly (two instructions per
    /// basic block — see `codegen::meter`), so the budget has to be *in* that
    /// field for native code to see it. Lending a bounded chunk rather than the
    /// whole budget is what makes a native run finite: it hands control back at
    /// most `NATIVE_CHUNK` steps later, which is the abort flag's worst-case
    /// response time inside an otherwise unbounded loop.
    ///
    /// The caller MUST pair this with [`Self::meter_return`], and must save and
    /// restore `jit_steps` around the pair — native code re-enters Rust through
    /// its call helpers, and that Rust can enter native again.
    #[must_use]
    pub(crate) fn meter_lend(&mut self) -> i64 {
        let Some(rec) = self.instr_rec.as_mut() else { return 0 };
        // Once per native entry is the right cadence for the abort poll: the
        // interpreter's own every-4096-instructions check barely advances while
        // a hot loop is running natively.
        if let Some(flag) = rec.abort.as_ref() {
            if flag.load(std::sync::atomic::Ordering::Relaxed) {
                self.jit_steps = 0;
                return 0;
            }
        }
        // The heap ceiling needs checking here for the same reason: compiled
        // code never reaches `instrument_step`, so a native loop that allocates
        // would run to the end of its lent chunk unexamined.
        let ceiling = rec.heap_limit;
        if ceiling != usize::MAX {
            if self.heap_bytes() > ceiling {
                // Lend nothing, so control returns to the interpreter, and
                // arrange for its next instruction to land on a poll — the
                // error itself is raised there, where there is a `Tick` to
                // return it in. Without this nudge the script could run
                // another 4095 instructions before anyone looked.
                if let Some(rec) = self.instr_rec.as_mut() {
                    rec.ticks = u64::MAX;
                }
                self.jit_steps = 0;
                return 0;
            }
            // `heap_bytes` took `&self`, so the recorder has to be re-borrowed.
            let Some(rec) = self.instr_rec.as_mut() else { return 0 };
            let lend = rec.remaining.min(NATIVE_CHUNK).max(0);
            rec.remaining -= lend;
            rec.used = rec.used.wrapping_add(lend as u64);
            self.jit_steps = lend;
            return lend;
        }
        let lend = rec.remaining.min(NATIVE_CHUNK).max(0);
        rec.remaining -= lend;
        rec.used = rec.used.wrapping_add(lend as u64);
        self.jit_steps = lend;
        lend
    }

    /// Reconcile after a native run: give back what it did not spend.
    ///
    /// A negative `jit_steps` is the overshoot of the block that tripped the
    /// check; it is absorbed rather than refunded, so the budget is never
    /// credited for work that happened.
    pub(crate) fn meter_return(&mut self) {
        let unspent = self.jit_steps.max(0);
        if let Some(rec) = self.instr_rec.as_mut() {
            if rec.remaining != i64::MAX {
                rec.remaining += unspent;
            }
            // The lend counted against `used` up front; hand back what native
            // code did not spend even when the budget is unlimited (where
            // `remaining` is not refunded), so `used` stays the count of work
            // that actually happened.
            rec.used = rec.used.saturating_sub(unspent as u64);
        }
    }

    /// Charge `n` steps for work that ran neither in the dispatch loop nor in
    /// compiled code — the off-frame method inliner evaluates a callee body in
    /// Rust, outside `run_loop` entirely, so nothing else would see it.
    ///
    /// Charge only AFTER the work is known to have completed: a path that
    /// declines half way falls back to a real call, which the interpreter
    /// charges itself, and charging both would be double-counting.
    pub(crate) fn charge_steps(&mut self, n: i64) {
        if let Some(rec) = self.instr_rec.as_mut() {
            if rec.remaining != i64::MAX {
                rec.remaining -= n;
            }
            rec.used = rec.used.wrapping_add(n.max(0) as u64);
        }
    }

    /// Whether the last native run stopped because it ran out of lent steps
    /// rather than because a type guard failed. A metering exit says nothing
    /// about the region's quality, so it must not count toward eviction.
    pub(crate) fn meter_exhausted(&self) -> bool {
        self.instr_rec.is_some() && self.jit_steps <= 0
    }

    /// The displacement of [`Self::jit_steps`] from the VM pointer, for the
    /// compilers to bake into `[rdi + off]`. `None` leaves the compiled code
    /// byte-identical to an unmetered build.
    pub(crate) fn meter_offset(&self) -> Option<i32> {
        self.instr_rec.as_ref()?;
        let base = self as *const Self as usize;
        let field = &self.jit_steps as *const i64 as usize;
        Some((field - base) as i32)
    }
}

impl super::Vm<'_> {
    /// The dispatch loop's hook, run once per instruction *before* it executes:
    /// completes the previous row, charges one step against the budget, polls
    /// the abort flag, and opens a row for `instr`.
    pub(crate) fn instrument_step(&mut self, base: usize, ip: usize, instr: &Instr) -> Tick {
        // Completing the previous row needs `&self.regs`, so it cannot happen
        // inside the `&mut self.instr_rec` borrow below.
        self.instrument_complete();

        let rec = match self.instr_rec.as_mut() {
            Some(r) => r,
            None => return Ok(()),
        };
        rec.ticks = rec.ticks.wrapping_add(1);
        if rec.remaining != i64::MAX {
            if rec.remaining <= 0 {
                return Err(BUDGET_MSG);
            }
            rec.remaining -= 1;
        }
        // Counted unconditionally — `used` is the host's consumption figure,
        // meaningful with or without a budget. The budget-exhausted return
        // above precedes this, so a stopped script reports exactly its cap.
        rec.used = rec.used.wrapping_add(1);
        // Heap ceiling rides the abort poll's schedule. Measuring it needs
        // `&self.heap`, which cannot be borrowed while `rec` is, so the limit
        // comes out here and the comparison happens once the borrow ends.
        let mut ceiling = usize::MAX;
        if rec.ticks & ABORT_CHECK_MASK == 0 {
            if let Some(flag) = rec.abort.as_ref() {
                if flag.load(std::sync::atomic::Ordering::Relaxed) {
                    return Err(ABORT_MSG);
                }
            }
            ceiling = rec.heap_limit;
        }
        let tracing = rec.tracing;

        if ceiling != usize::MAX && self.heap_bytes() > ceiling {
            return Err(MEMORY_MSG);
        }

        let Some(rec) = self.instr_rec.as_mut() else {
            return Ok(());
        };
        if !tracing {
            return Ok(());
        }
        if rec.steps.len() >= rec.max_steps {
            rec.truncated = true;
            rec.tracing = false;
            return Ok(());
        }
        self.instrument_open(base, ip, instr);
        Ok(())
    }

    /// Fill in the previous row's `val_dst` and confirm — or drop — its claim.
    fn instrument_complete(&mut self) {
        let Some(rec) = self.instr_rec.as_ref() else {
            return;
        };
        let Some(p) = rec.pending.as_ref() else {
            return;
        };
        let (row_idx, claim, dst, pre_frames) = (p.row, p.claim, p.dst, p.frames);

        // Reading the register file has to happen before the &mut borrow.
        let dst_val = dst.and_then(|i| self.regs.get(i).copied());
        let frames_now = self.frames.len();

        let rec = self.instr_rec.as_mut().expect("checked above");
        rec.pending = None;
        let depth_before = rec.depth;
        let row = &mut rec.steps[row_idx];
        let dst_u64 = dst_val.map(enc).unwrap_or(0);

        let ok = match claim {
            Claim::Const => {
                row.val_dst = dst_u64;
                row.const_val = dst_u64;
                true
            }
            // Both columns encode the same JS value, so the identity holds for
            // any deterministic encoding — including the hashed fallback.
            Claim::Move | Claim::GetGlobal => {
                row.val_dst = dst_u64;
                row.val_a = dst_u64;
                true
            }
            Claim::SetGlobal => {
                row.val_dst = 0;
                true
            }
            Claim::Arith(kind) => match (
                exact_uint_col(row.val_a),
                exact_uint_col(row.val_b),
                dst_val.and_then(exact_uint),
            ) {
                (Some(a), Some(b), Some(d)) => {
                    row.val_dst = d;
                    match kind {
                        op::ADD => a + b == d,
                        op::SUB => a >= b && a - b == d,
                        op::MUL => a.checked_mul(b) == Some(d),
                        // The AIR checks `val_a == val_dst * val_b`: exact
                        // division only, which is what JS `/` gives here when
                        // the result is an integer.
                        op::DIV => b != 0 && d.checked_mul(b) == Some(a),
                        // `val_a == val_b * aux + val_dst`, with `aux` the
                        // quotient. `d < b` is the remainder range check the
                        // AIR does not do for us.
                        op::MOD => {
                            if b != 0 && d < b {
                                row.aux = a / b;
                                row.aux * b + d == a
                            } else {
                                false
                            }
                        }
                        _ => false,
                    }
                }
                _ => false,
            },
            // `val_dst + val_a == 0` over the field. A u64 encoding has no
            // negatives, so only the zero case survives.
            Claim::Neg => {
                row.val_dst = dst_u64;
                row.val_a == 0 && dst_u64 == 0
            }
            Claim::Not => match dst_val {
                Some(v) if v.is_bool() => {
                    row.val_dst = v.as_bool() as u64;
                    true
                }
                _ => false,
            },
            Claim::Cmp => match dst_val {
                Some(v) if v.is_bool() => {
                    row.aux = v.as_bool() as u64;
                    row.val_dst = row.aux;
                    true
                }
                _ => false,
            },
            // The AIR reconstructs `val_dst` from eight bit columns, so only a
            // result that fits in a byte can be claimed.
            Claim::Bitwise => match dst_val.and_then(exact_uint) {
                Some(d) if d < 256 => {
                    row.val_dst = d;
                    true
                }
                _ => false,
            },
            // `aux` (the next row's pc) is filled in by `instrument_open`.
            Claim::Jump => true,
            Claim::Call => {
                row.val_dst = 0;
                // A call that dispatched to a native builtin pushes no frame,
                // so the depth column would never come back down.
                frames_now > pre_frames
            }
            Claim::Return => {
                row.val_dst = 0;
                // Never let the depth column go negative: the AIR requires
                // `depth[i+1] == depth[i] - 1` on a RETURN row, and the prover
                // recomputes that column from these very opcodes.
                frames_now < pre_frames && depth_before > 0
            }
            Claim::Flat | Claim::None => {
                row.val_dst = dst_u64;
                true
            }
        };

        if ok {
            match claim {
                Claim::Call => rec.depth += 1,
                Claim::Return => rec.depth -= 1,
                _ => {}
            }
        } else {
            blank(&mut rec.steps[row_idx]);
        }
    }

    /// Open a row for `instr`: classify it and sample its operand registers.
    fn instrument_open(&mut self, base: usize, ip: usize, instr: &Instr) {
        let (opcode, claim, a, b, cst, dst) = classify(instr);
        let val_a = a.map(|r| enc(self.reg_at(base, r))).unwrap_or(0);
        let val_b = b.map(|r| enc(self.reg_at(base, r))).unwrap_or(0);
        let frames = self.frames.len();
        let rec = self.instr_rec.as_mut().expect("caller checked");
        let row = rec.steps.len();
        // A jump claims that THIS row's pc is its `aux`.
        if row > 0 && rec.steps[row - 1].opcode == op::JUMP {
            rec.steps[row - 1].aux = ip as u64;
        }
        rec.steps.push(TraceStep {
            clk: row as u64,
            pc: ip as u64,
            opcode,
            val_a,
            val_b,
            val_dst: 0,
            const_val: cst,
            aux: 0,
        });
        rec.pending = Some(Pending {
            row,
            dst: dst.map(|r| base + r as usize),
            frames,
            claim,
        });
    }

    fn reg_at(&self, base: usize, r: u16) -> Value {
        self.regs
            .get(base + r as usize)
            .copied()
            .unwrap_or(Value::UNDEFINED)
    }
}

/// Exact non-negative integer value of `v`, if it has one below 2^32.
///
/// The bound is what keeps the arithmetic claims exact: the prover's field is
/// 128-bit, so a product of two values under 2^32 never wraps and a sum never
/// aliases a different pair.
fn exact_uint(v: Value) -> Option<u64> {
    if v.is_int() {
        let i = v.as_int();
        return if i >= 0 { Some(i as u64) } else { None };
    }
    if v.is_double() {
        let d = v.as_f64();
        // `d.fract() == 0.0` is true for -0.0 too, which `d >= 0.0` admits and
        // which encodes to 0 — correct, since -0 and +0 are `===` in JS.
        if d >= 0.0 && d.fract() == 0.0 && d < 4_294_967_296.0 {
            return Some(d as u64);
        }
    }
    None
}

/// [`exact_uint`] over an already-encoded operand column. Encoded small integers
/// are stored as themselves, and the hashed fallback is always ≥ 2^62, so the
/// round trip is exact and a heap value can never be mistaken for an integer.
fn exact_uint_col(encoded: u64) -> Option<u64> {
    (encoded < 4_294_967_296).then_some(encoded)
}

/// Field-safe encoding of a JS value for the trace's value columns.
///
/// Non-negative integers below 2^32 encode as themselves, so arithmetic claims
/// over them are literally true. Everything else — negative numbers, fractions,
/// strings, objects — encodes to a value at or above 2^62 derived from the
/// NaN-boxed bits: enough for the equality claims (MOVE, GET_GLOBAL, CONST),
/// and deliberately out of range for the arithmetic ones.
///
/// A heap value's bits are a heap INDEX, and the collector recycles indices —
/// so two rows with equal heap-tagged encodings at different clocks need not be
/// the same object. No constraint in the AIR reads across rows, so this is a
/// limit on what the trace means, not a soundness hole.
fn enc(v: Value) -> u64 {
    if let Some(n) = exact_uint(v) {
        return n;
    }
    // SplitMix64 finalizer over the raw bits, forced above the integer range.
    let mut z = v.bits().wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    // Below 2^63 so the field-element conversion is trivially in range, and at
    // or above 2^62 so `exact_uint_col` never admits it.
    (z >> 2) | (1 << 62)
}

/// Map a zipp instruction onto a trace opcode, the identity it may claim, and
/// the registers to sample: `(opcode, claim, a, b, const_val, dst)`.
///
/// Deliberately conservative. An instruction whose effect the AIR cannot express
/// is [`op::OTHER`], which attests only that a step occurred at this clock —
/// real (the row is bound to the step count, the clock discipline and the
/// program hash) but silent about values. Adding a case here is how the proof
/// gets stronger; guessing one is how it becomes false.
#[allow(clippy::type_complexity)]
fn classify(instr: &Instr) -> (u8, Claim, Option<u16>, Option<u16>, u64, Option<u16>) {
    use Instr::*;
    match *instr {
        // ── constants and moves ──
        LoadConst { dst, .. }
        | LoadInt { dst, .. }
        | LoadUndefined { dst }
        | LoadNull { dst }
        | LoadBool { dst, .. }
        | LoadHole { dst }
        | LoadBigInt { dst, .. } => (op::CONST, Claim::Const, None, None, 0, Some(dst)),
        Move { dst, src } => (op::MOVE, Claim::Move, Some(src), None, 0, Some(dst)),

        // ── globals: half of the memory bus ──
        LoadGlobal { dst, idx } | LoadGlobalOrUndefined { dst, idx } => (
            op::GET_GLOBAL,
            Claim::GetGlobal,
            None,
            None,
            idx as u64,
            Some(dst),
        ),
        StoreGlobal { idx, src } | StoreGlobalStrict { idx, src } => (
            op::SET_GLOBAL,
            Claim::SetGlobal,
            Some(src),
            None,
            idx as u64,
            None,
        ),

        // ── arithmetic ──
        // StrConcat/StrAppendInPlace are `Add` with a JIT routing hint; they are
        // the same operator and the post-check decides whether the row's values
        // actually satisfy addition.
        Add { dst, a, b } | StrConcat { dst, a, b } | StrAppendInPlace { dst, a, b } => (
            op::ADD,
            Claim::Arith(op::ADD),
            Some(a),
            Some(b),
            0,
            Some(dst),
        ),
        Sub { dst, a, b } => (
            op::SUB,
            Claim::Arith(op::SUB),
            Some(a),
            Some(b),
            0,
            Some(dst),
        ),
        Mul { dst, a, b } => (
            op::MUL,
            Claim::Arith(op::MUL),
            Some(a),
            Some(b),
            0,
            Some(dst),
        ),
        Div { dst, a, b } => (
            op::DIV,
            Claim::Arith(op::DIV),
            Some(a),
            Some(b),
            0,
            Some(dst),
        ),
        Mod { dst, a, b } => (
            op::MOD,
            Claim::Arith(op::MOD),
            Some(a),
            Some(b),
            0,
            Some(dst),
        ),
        Pow { dst, a, b } => (op::POW, Claim::None, Some(a), Some(b), 0, Some(dst)),
        Neg { dst, a } => (op::NEG, Claim::Neg, Some(a), None, 0, Some(dst)),
        Bitwise { dst, a, b, .. } => (op::BITWISE, Claim::Bitwise, Some(a), Some(b), 0, Some(dst)),
        BitNot { dst, a } => (op::BITWISE, Claim::Bitwise, Some(a), None, 0, Some(dst)),
        Not { dst, a } => (op::NOT, Claim::Not, Some(a), None, 0, Some(dst)),

        // ── comparisons ──
        Lt { dst, a, b }
        | Le { dst, a, b }
        | Gt { dst, a, b }
        | Ge { dst, a, b }
        | Eq { dst, a, b }
        | Ne { dst, a, b }
        | LooseEq { dst, a, b }
        | LooseNe { dst, a, b } => (op::CMP, Claim::Cmp, Some(a), Some(b), 0, Some(dst)),

        // ── control flow ──
        Jump { .. } => (op::JUMP, Claim::Jump, None, None, 0, None),
        JumpIfFalse { cond, .. } | JumpIfTrue { cond, .. } => {
            (op::JUMP, Claim::Jump, Some(cond), None, 0, None)
        }
        JumpIfNotLt { a, b, .. } | JumpIfNotLe { a, b, .. } => {
            (op::JUMP, Claim::Jump, Some(a), Some(b), 0, None)
        }

        // ── calls and returns ──
        Call { dst, callee, .. } | CallWithThis { dst, callee, .. } => {
            (op::CALL, Claim::Call, Some(callee), None, 0, Some(dst))
        }
        New { dst, callee, .. } => (op::CALL, Claim::Call, Some(callee), None, 0, Some(dst)),
        CallMethod { dst, obj, name, .. } => (
            op::CALL,
            Claim::Call,
            Some(obj),
            None,
            name as u64,
            Some(dst),
        ),
        CallMethodComputed { dst, obj, key, .. } => {
            (op::CALL, Claim::Call, Some(obj), Some(key), 0, Some(dst))
        }
        Return { src } => (op::RETURN, Claim::Return, Some(src), None, 0, None),
        ReturnUndefined => (op::RETURN, Claim::Return, None, None, 0, None),

        // ── property access: the other half of the memory bus ──
        GetProp { dst, obj, name } => (
            op::PROP,
            Claim::Flat,
            Some(obj),
            None,
            name as u64,
            Some(dst),
        ),
        SetProp { obj, name, val, .. } | InitDataProp { obj, name, val } => (
            op::PROP,
            Claim::Flat,
            Some(obj),
            Some(val),
            name as u64,
            None,
        ),
        AppendDataProp { obj, name, val } => (
            op::PROP,
            Claim::Flat,
            Some(obj),
            Some(val),
            name as u64,
            None,
        ),
        GetIndex { dst, obj, key } => (op::PROP, Claim::Flat, Some(obj), Some(key), 0, Some(dst)),
        SetIndex { obj, key, val } => (op::PROP, Claim::Flat, Some(obj), Some(key), 0, Some(val)),

        // ── collection construction ──
        NewArray { dst, .. } | NewObject { dst, .. } | NewMap { dst, .. } | NewSet { dst, .. } => {
            (op::COLLECTION, Claim::Flat, None, None, 0, Some(dst))
        }

        // Everything else attests only that a step happened here.
        _ => (op::OTHER, Claim::None, None, None, 0, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The opcode numbers are a wire contract with the prover crate. Change one
    /// and a row starts being held to a DIFFERENT polynomial constraint —
    /// previously valid proofs stop verifying, with no compile error anywhere,
    /// across two repositories. Pin them.
    #[test]
    fn opcode_numbering_is_stable() {
        let expected: [u8; 21] = core::array::from_fn(|i| i as u8);
        assert_eq!(
            [
                op::CONST,
                op::MOVE,
                op::GET_GLOBAL,
                op::SET_GLOBAL,
                op::ADD,
                op::SUB,
                op::MUL,
                op::DIV,
                op::MOD,
                op::POW,
                op::NEG,
                op::NOT,
                op::CMP,
                op::BITWISE,
                op::JUMP,
                op::CALL,
                op::RETURN,
                op::PROP,
                op::COLLECTION,
                op::HALT,
                op::OTHER,
            ],
            expected,
        );
        assert_eq!(op::COUNT, 21);
    }

    /// The hashed fallback must never be mistaken for a small integer, or a row
    /// carrying a string would be admitted to an arithmetic constraint.
    #[test]
    fn hashed_encoding_never_looks_like_an_integer() {
        for v in [
            Value::UNDEFINED,
            Value::NULL,
            Value::TRUE,
            Value::heap(0),
            Value::heap(u32::MAX - 1),
            Value::int(-1),
            Value::num(2.5),
            Value::num(f64::NAN),
            Value::num(-4_294_967_296.0),
        ] {
            let e = enc(v);
            assert!(exact_uint(v).is_none() || exact_uint_col(e).is_some());
            if exact_uint(v).is_none() {
                assert!(exact_uint_col(e).is_none(), "{v:?} encoded to {e}");
                assert!(
                    e >= 1 << 62 && e < 1 << 63,
                    "{v:?} encoded out of band: {e}"
                );
            }
        }
    }

    #[test]
    fn integers_in_range_encode_to_themselves() {
        for i in [0i32, 1, 2, 1000, i32::MAX] {
            assert_eq!(enc(Value::int(i)), i as u64);
        }
        assert_eq!(enc(Value::num(7.0)), 7);
        // Negatives, fractions and 2^32-and-up have no exact in-range form.
        assert!(exact_uint(Value::int(-1)).is_none());
        assert!(exact_uint(Value::num(2.5)).is_none());
        assert!(exact_uint(Value::num(4_294_967_296.0)).is_none());
    }
}
