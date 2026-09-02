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
//! (`Vm::enter_trace_mode` in full instrumentation builds). A trace has to be a row-per-instruction
//! record; native code produces no rows, so a JIT'd hot loop would simply be
//! missing from it while the program still returned the right answer — and a
//! proof over that trace would attest to an execution that never happened.
//!
//! Two paths execute user work outside the interpreter's pre-execution charge:
//! the fused array kernels and the off-frame method inliner. Both are declined
//! outright in a metered VM (`Vm::jit_fused_ok` for kernels and the inliner's
//! own entry gate), so neither can overshoot or omit nested bytecode work.
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
#[cfg(not(feature = "meter-only"))]
use crate::value::Value;

/// Without a JIT tier there is no native code to lend budget to; the constant
/// is only referenced by the metering loan path, which no build without
/// `codegen` can reach. Keep the value in step with `codegen::meter` anyway, so
/// the two never silently disagree about the unit.
#[cfg(all(test, not(all(feature = "jit", target_arch = "x86_64"))))]
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
#[cfg(not(feature = "meter-only"))]
pub(crate) const ABORT_MSG: &str = "RangeError: script execution was aborted by the host";
/// Raised when a script exceeds the heap ceiling its host set.
pub(crate) const MEMORY_MSG: &str = "RangeError: script exceeded its memory budget";
pub(crate) const OUTPUT_MSG: &str = "RangeError: script exceeded its output budget";

/// What one buffered console line costs beyond the text it carries.
///
/// A line is retained as a `String` in a `Vec` and later handed to the host as
/// one node of the array `takeOutput` returns, so its cost is never zero: there
/// is a pointer, a length, a capacity and a slot whatever the line says.
///
/// Charging `len + 1` made an EMPTY line cost a single byte, so an 8 MiB budget
/// admitted 8.4 million of them — over a hundred megabytes of retained entries,
/// and four times as many nodes as the host boundary will convert. Neither
/// guard was reached in between. `while (true) console.log("")` simply grew
/// until the WebAssembly instance trapped on `unreachable`, which the host
/// cannot catch, cannot report, and cannot tell apart from a bug in the engine:
///
/// ```text
/// 3,800,000 empty lines -> ok
/// 3,900,000 empty lines -> RuntimeError: unreachable
/// ```
///
/// Eight bytes holds the entry count to `output_limit / 8`, which stays under
/// the host's node cap so `takeOutput` can always return what was buffered,
/// and leaves the byte budget to bound real text as before. A line with
/// content is charged what it always was, plus this.
pub(crate) const OUTPUT_LINE_OVERHEAD_BYTES: usize = 8;
pub(crate) const DYNAMIC_SOURCE_MSG: &str =
    "RangeError: dynamic code source exceeds its per-compilation limit";
pub(crate) const DYNAMIC_TOTAL_SOURCE_MSG: &str =
    "RangeError: dynamic code exceeded its lifetime source limit";
pub(crate) const DYNAMIC_CALLS_MSG: &str =
    "RangeError: dynamic code exceeded its lifetime compilation-call limit";
pub(crate) const DYNAMIC_FUNCTIONS_MSG: &str =
    "RangeError: dynamic code exceeded its retained-function limit";
pub(crate) const DYNAMIC_CLASSES_MSG: &str =
    "RangeError: dynamic code exceeded its retained-class limit";
#[cfg(feature = "safe-sandbox")]
pub(crate) const REGEX_STEPS_MSG: &str =
    "RangeError: regular expression exceeded its execution budget";
#[cfg(feature = "safe-sandbox")]
pub(crate) const REGEX_MEMORY_MSG: &str =
    "RangeError: regular expression exceeded its backtrack memory budget";

/// The first resource ceiling an instrumented VM actually attempted to cross.
///
/// This is deliberately typed and sticky. In particular, `remaining == 0` is
/// not itself an error: a program may consume its final permitted instruction
/// and halt successfully. `Steps` is recorded only when another instruction is
/// attempted. Keeping the first cause also prevents a guest-thrown string from
/// impersonating a host resource failure and makes caught/promise-wrapped
/// failures visible to the embedder after execution returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResourceExhaustion {
    Steps,
    #[cfg(not(feature = "meter-only"))]
    Abort,
    Heap,
    Output,
    DynamicSource,
    DynamicTotalSource,
    DynamicCalls,
    DynamicFunctions,
    DynamicClasses,
    #[cfg(feature = "safe-sandbox")]
    RegexSteps,
    #[cfg(feature = "safe-sandbox")]
    RegexMemory,
}

impl ResourceExhaustion {
    pub(crate) const fn message(self) -> &'static str {
        match self {
            Self::Steps => BUDGET_MSG,
            #[cfg(not(feature = "meter-only"))]
            Self::Abort => ABORT_MSG,
            Self::Heap => MEMORY_MSG,
            Self::Output => OUTPUT_MSG,
            Self::DynamicSource => DYNAMIC_SOURCE_MSG,
            Self::DynamicTotalSource => DYNAMIC_TOTAL_SOURCE_MSG,
            Self::DynamicCalls => DYNAMIC_CALLS_MSG,
            Self::DynamicFunctions => DYNAMIC_FUNCTIONS_MSG,
            Self::DynamicClasses => DYNAMIC_CLASSES_MSG,
            #[cfg(feature = "safe-sandbox")]
            Self::RegexSteps => REGEX_STEPS_MSG,
            #[cfg(feature = "safe-sandbox")]
            Self::RegexMemory => REGEX_MEMORY_MSG,
        }
    }
}

#[derive(Clone, Copy)]
struct DynamicCodeLimits {
    per_source_bytes: usize,
    lifetime_source_bytes: usize,
    calls: usize,
    functions: usize,
    classes: usize,
}

impl DynamicCodeLimits {
    const UNLIMITED: Self = Self {
        per_source_bytes: usize::MAX,
        lifetime_source_bytes: usize::MAX,
        calls: usize::MAX,
        functions: usize::MAX,
        classes: usize::MAX,
    };
}

/// A row whose operands have been sampled but whose result has not: the
/// destination register is not written until the instruction runs, so the row is
/// completed at the top of the next dispatch iteration.
#[cfg(not(feature = "meter-only"))]
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
#[cfg(not(feature = "meter-only"))]
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
#[cfg(not(feature = "meter-only"))]
const ABORT_CHECK_MASK: u64 = 0xFFF;
/// A full payload reconciliation walks the heap, so do it less often than the
/// cheap abort poll. New objects and known large buffers are charged eagerly;
/// this audit catches capacity growth of already-existing collections.
const HEAP_AUDIT_MASK: u64 = 0xFFFF;
#[cfg(any(
    all(feature = "meter-only", not(feature = "jit"), not(test)),
    all(feature = "meter-only", test)
))]
const HEAP_AUDIT_INTERVAL: u64 = HEAP_AUDIT_MASK + 1;

/// Host re-entries between forced full heap audits in the postflight. The
/// interpreter reconciles on a tick stride; a host that re-enters without
/// running bytecode ticks nothing, so it needs a stride of its own or in-place
/// capacity growth would go uncounted indefinitely. 256 keeps the amortised
/// cost near zero while bounding the drift window to a fixed number of calls
/// rather than to whatever the embedding happens to do.
const POSTFLIGHT_AUDIT_STRIDE: u32 = 256;

/// Preflight calls between forced full heap audits. The preflight sits on the
/// per-part string paths — `JSON.stringify`, `join`, the string builders, regex
/// results — so it runs orders of magnitude more often than either the tick
/// stride or the boundary stride, and it is the one place where an unconditional
/// walk turns linear work quadratic. Same bargain as `POSTFLIGHT_AUDIT_STRIDE`,
/// same window.
const PREFLIGHT_AUDIT_STRIDE: u32 = 256;

/// Periodic polls between forced full heap walks in the dispatch loop. Ordinary
/// and unlimited meters poll every 65,536 dispatches; the finite release-wasm
/// countdown polls on the first dispatch after each 65,536 metered-step
/// threshold (off-loop charges can make that earlier in dispatch terms). The
/// poll still answers from the O(1) figure every time; this is only how often
/// exact reconciliation runs, so in-place capacity growth remains bounded
/// without the walk riding every instruction.
const HEAP_WALK_STRIDE: u32 = 64;

/// Polls between reconciliation walks, by heap size in slots. The walk is
/// O(slots); below a few thousand it is cheaper than the poll's own
/// bookkeeping, so small heaps -- where in-place payload growth is the whole
/// story -- reconcile every time.
fn heap_walk_stride_for(slots: usize) -> u32 {
    if slots < 4_096 {
        1
    } else if slots < 65_536 {
        8
    } else {
        HEAP_WALK_STRIDE
    }
}

/// Per-VM instrumentation state. Allocated only when a host asks for it.
pub(crate) struct Recorder {
    /// Instructions the script may still execute, NOT counting any chunk
    /// currently lent to native code (see `Vm::meter_lend`). Signed because the
    /// native tier charges a whole basic block at a time and can overshoot zero
    /// by up to one block; `i64::MAX` means unlimited.
    pub(crate) remaining: i64,
    /// Instructions executed outside the interpreter dispatch loop: the native
    /// tier's net lending (lent at `meter_lend`, unspent refunded at
    /// `meter_return`) and off-loop work charged through `charge_steps`.
    /// Interpreter work is already counted by `ticks`; [`Self::steps_used`]
    /// combines both counters at the observation boundary.
    ///
    /// The finite, no-JIT release wasm profile instead stores the next
    /// `remaining` balance at which a heap poll is due. Its initial balance in
    /// `ticks` makes a separate off-loop counter unnecessary, and reusing this
    /// field avoids adding another hot-path load or another recorder field.
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
    /// Combined UTF-8 bytes retained for stdout/stderr lines, including the
    /// newline written for each line; `usize::MAX` means unlimited.
    pub(crate) output_limit: usize,
    pub(crate) output_used: usize,
    pub(crate) output_exhausted: bool,
    /// First resource ceiling actually crossed. Never inferred from exception
    /// text or from a merely-empty step balance.
    pub(crate) exhaustion: Option<ResourceExhaustion>,
    dynamic_limits: DynamicCodeLimits,
    dynamic_calls: usize,
    dynamic_source_bytes: usize,
    #[cfg(not(feature = "meter-only"))]
    steps: Vec<TraceStep>,
    /// Whether to record rows at all — metering works without tracing.
    #[cfg(not(feature = "meter-only"))]
    tracing: bool,
    /// Stop recording (but keep executing) past this many rows. A truncated
    /// trace is unprovable, which [`Recorder::finish`] reports as `None`.
    #[cfg(not(feature = "meter-only"))]
    max_steps: usize,
    #[cfg(not(feature = "meter-only"))]
    truncated: bool,
    #[cfg(not(feature = "meter-only"))]
    pending: Option<Pending>,
    /// The call depth the emitted rows imply. The prover recomputes this from
    /// the opcodes, so the two must agree — never emit a RETURN at depth 0.
    #[cfg(not(feature = "meter-only"))]
    depth: u64,
    /// Interpreter-dispatch clock in ordinary instrumentation builds and for
    /// unlimited meters. In the finite, no-JIT release wasm profile this holds
    /// the immutable initial step ceiling, so total work is derived as
    /// `ticks - remaining` without writing a second counter per instruction.
    ticks: u64,
    /// Host re-entries since the last full heap audit. The postflight check is
    /// O(1) and reads a figure that lags in-place capacity growth, so it is
    /// reconciled on a fixed stride the way the interpreter reconciles on
    /// `HEAP_AUDIT_MASK` ticks. Without it a workload that only ever crosses
    /// the host boundary — which is what SoftN does between frames — would
    /// never reconcile at all.
    postflight_since_audit: u32,
    /// Preflight calls since the last full audit. Starts at the stride so the
    /// very first preflight audits: `Heap::audit_resident_bytes` is what turns
    /// payload accounting on, and until it has run once the O(1) estimate omits
    /// object payload entirely and would read far too low to be trusted.
    preflight_since_audit: u32,
    /// `HEAP_AUDIT_MASK` polls since the last full walk in the dispatch loop.
    /// Starts at the stride so the first poll reconciles, which is also what
    /// switches payload accounting on.
    ticks_since_heap_walk: u32,
    /// Retained growth in a VM-owned side table that is deliberately absent
    /// from `Heap::resident_bytes` (private fields/brands are the motivating
    /// case). The next interpreter poll or host-boundary postflight must run
    /// one exact VM audit; non-allocating re-entry keeps the O(1) fast path.
    external_heap_dirty: bool,
    /// Non-`Heap` resident bytes observed by the latest exact audit. Cheap
    /// ceiling checks add the live `Heap` high-water figure, retaining the
    /// VM-core/side-table baseline without another heap-slot walk. Zero before
    /// the first audit safely falls back to the historical Heap-only estimate.
    pub(crate) heap_audit_non_heap: std::cell::Cell<usize>,
}

impl Recorder {
    pub(crate) fn new() -> Self {
        Self {
            remaining: i64::MAX,
            used: 0,
            abort: None,
            heap_limit: usize::MAX,
            output_limit: usize::MAX,
            output_used: 0,
            output_exhausted: false,
            exhaustion: None,
            postflight_since_audit: 0,
            preflight_since_audit: PREFLIGHT_AUDIT_STRIDE,
            ticks_since_heap_walk: HEAP_WALK_STRIDE,
            external_heap_dirty: false,
            heap_audit_non_heap: std::cell::Cell::new(0),
            dynamic_limits: DynamicCodeLimits::UNLIMITED,
            dynamic_calls: 0,
            dynamic_source_bytes: 0,
            #[cfg(not(feature = "meter-only"))]
            steps: Vec::new(),
            #[cfg(not(feature = "meter-only"))]
            tracing: false,
            #[cfg(not(feature = "meter-only"))]
            max_steps: usize::MAX,
            #[cfg(not(feature = "meter-only"))]
            truncated: false,
            #[cfg(not(feature = "meter-only"))]
            pending: None,
            #[cfg(not(feature = "meter-only"))]
            depth: 0,
            ticks: 0,
        }
    }

    /// Begin recording. `max_steps` bounds memory: at ~64 bytes a row, a hot
    /// loop would otherwise exhaust the host long before it finished.
    #[cfg(not(feature = "meter-only"))]
    pub(crate) fn start_trace(&mut self, max_steps: usize) {
        self.steps.clear();
        self.steps.reserve(max_steps.min(1 << 16));
        self.tracing = true;
        self.max_steps = max_steps;
        self.truncated = false;
        self.pending = None;
        self.depth = 0;
    }

    #[cfg(not(feature = "meter-only"))]
    pub(crate) fn truncated(&self) -> bool {
        self.truncated
    }

    /// Install a fresh step ceiling. The release wasm meter reuses `ticks` as
    /// the immutable finite starting balance; all other profiles keep its
    /// historical role as the interpreter-dispatch counter.
    pub(crate) fn set_step_limit(&mut self, max_steps: u64) {
        // Saturating, so `u64::MAX` keeps meaning "unlimited".
        self.remaining = max_steps.min(i64::MAX as u64) as i64;
        #[cfg(all(feature = "meter-only", not(feature = "jit"), not(test)))]
        {
            self.ticks = if self.remaining == i64::MAX {
                0
            } else {
                self.remaining as u64
            };
            self.used = finite_next_heap_poll_balance(self.ticks);
        }
    }

    /// Total metered work. Ordinary and unlimited profiles combine disjoint
    /// dispatch/off-loop counters. A finite release-wasm meter instead derives
    /// the total from its immutable initial balance and current countdown.
    #[inline]
    pub(crate) fn steps_used(&self) -> u64 {
        #[cfg(all(feature = "meter-only", not(feature = "jit"), not(test)))]
        if self.remaining != i64::MAX {
            return finite_meter_total_used(self.ticks, self.remaining);
        }
        self.used.wrapping_add(self.ticks)
    }

    #[inline]
    fn exhaust(&mut self, cause: ResourceExhaustion) -> &'static str {
        self.exhaustion.get_or_insert(cause).message()
    }

    #[inline]
    fn terminal_message(&self) -> Option<&'static str> {
        self.exhaustion.map(ResourceExhaustion::message)
    }

    fn set_dynamic_code_limits(
        &mut self,
        per_source_bytes: usize,
        lifetime_source_bytes: usize,
        calls: usize,
        functions: usize,
        classes: usize,
    ) {
        self.dynamic_limits = DynamicCodeLimits {
            per_source_bytes,
            lifetime_source_bytes,
            calls,
            functions,
            classes,
        };
        self.dynamic_calls = 0;
        self.dynamic_source_bytes = 0;
    }

    /// Charge a dynamic compilation attempt before parsing or compiler
    /// allocation. Failed parses count too: otherwise a guest can repeatedly
    /// spend parser work without consuming the lifetime allowance.
    fn charge_dynamic_code_attempt(&mut self, source_bytes: usize) -> Tick {
        if let Some(message) = self.terminal_message() {
            return Err(message);
        }
        if source_bytes > self.dynamic_limits.per_source_bytes {
            return Err(self.exhaust(ResourceExhaustion::DynamicSource));
        }
        if self.dynamic_calls >= self.dynamic_limits.calls {
            return Err(self.exhaust(ResourceExhaustion::DynamicCalls));
        }
        let Some(total) = self.dynamic_source_bytes.checked_add(source_bytes) else {
            return Err(self.exhaust(ResourceExhaustion::DynamicTotalSource));
        };
        if total > self.dynamic_limits.lifetime_source_bytes {
            return Err(self.exhaust(ResourceExhaustion::DynamicTotalSource));
        }
        self.dynamic_calls += 1;
        self.dynamic_source_bytes = total;
        Ok(())
    }

    /// Preflight the concrete stable-address allocations produced by a
    /// successful dynamic compile. The caller passes the already-installed
    /// counts plus the pending program's counts, and performs this check before
    /// any `Box::leak`.
    fn admit_dynamic_code_install(
        &mut self,
        installed_functions: usize,
        new_functions: usize,
        installed_classes: usize,
        new_classes: usize,
    ) -> Tick {
        if let Some(message) = self.terminal_message() {
            return Err(message);
        }
        if installed_functions
            .checked_add(new_functions)
            .is_none_or(|n| n > self.dynamic_limits.functions)
        {
            return Err(self.exhaust(ResourceExhaustion::DynamicFunctions));
        }
        if installed_classes
            .checked_add(new_classes)
            .is_none_or(|n| n > self.dynamic_limits.classes)
        {
            return Err(self.exhaust(ResourceExhaustion::DynamicClasses));
        }
        Ok(())
    }

    /// Stop recording and hand over the rows, terminated by a [`op::HALT`] row
    /// carrying `result` — the value the prover's boundary assertion pins.
    ///
    /// `None` means the trace is unusable and the caller must fall back to an
    /// unproven receipt: it was truncated, or it is too short to satisfy the
    /// AIR's boundary assertions (row 0 is asserted NOT to be the halt row).
    #[cfg(not(feature = "meter-only"))]
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

/// Recover total work from the finite release-wasm countdown. A negative
/// balance means completed off-loop work crossed the ceiling; public
/// accounting remains capped at that ceiling, and subtraction cannot wrap even
/// if the sticky exhaustion is observed after the charge.
#[cfg(any(
    all(feature = "meter-only", not(feature = "jit"), not(test)),
    all(feature = "meter-only", test)
))]
#[inline(always)]
fn finite_meter_total_used(initial: u64, remaining: i64) -> u64 {
    let remaining = (remaining.max(0) as u64).min(initial);
    initial.saturating_sub(remaining)
}

#[cfg(any(
    all(feature = "meter-only", not(feature = "jit"), not(test)),
    all(feature = "meter-only", test)
))]
#[inline(always)]
fn finite_next_heap_poll_balance(balance: u64) -> u64 {
    balance.saturating_sub(HEAP_AUDIT_INTERVAL)
}

/// Strip a row back to a bare "a step happened here" claim.
#[cfg(not(feature = "meter-only"))]
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

/// A scoped charge for native RegExp result storage that survives across a
/// guest callback. The counter is shared instead of borrowed so the callback
/// may freely re-enter the same VM; dropping this guard releases the charge on
/// success, error propagation, and panic unwinding alike.
#[cfg(feature = "safe-sandbox")]
pub(crate) struct RegexTransientReservation {
    counter: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    bytes: usize,
}

#[cfg(feature = "safe-sandbox")]
impl Drop for RegexTransientReservation {
    fn drop(&mut self) {
        use std::sync::atomic::Ordering;
        let previous = self.counter.fetch_sub(self.bytes, Ordering::Relaxed);
        debug_assert!(
            previous >= self.bytes,
            "RegExp transient-memory reservation underflow"
        );
    }
}

impl super::Vm<'_> {
    /// O(1) total-resident estimate between exact audits. `Heap` maintains a
    /// monotonic resident high-water figure, while allocations in VM-owned
    /// side tables request an exact audit through `external_heap_dirty`.
    #[inline]
    fn instrument_heap_estimate(&self) -> usize {
        let heap_now = self.heap.resident_bytes();
        let Some(rec) = self.instr_rec.as_ref() else {
            return heap_now;
        };
        heap_now.saturating_add(rec.heap_audit_non_heap.get())
    }

    /// Derive one regex search's hard ceilings. The fixed ceilings stop
    /// catastrophic backtracking even when the host left its VM budgets
    /// unlimited; finite VM budgets tighten them so transient backtrack
    /// storage cannot hide outside `heap_bytes()` and regex work cannot spend
    /// more gas than remains.
    #[cfg(feature = "safe-sandbox")]
    pub(crate) fn instrument_regex_limits(&self) -> regress::MatchLimits {
        use std::sync::atomic::Ordering;

        let mut limits = regress::MatchLimits::SANDBOX;
        // `heap_bytes()` deliberately measures VM-owned resident storage, not
        // Rust locals in an outer regex operation. Subtract their scoped
        // reservation independently from the fixed regex ceiling so nesting is
        // bounded even when the embedder left its heap limit unlimited.
        let transient = self.regex_transient_bytes.load(Ordering::Relaxed);
        limits.max_memory_bytes = limits.max_memory_bytes.saturating_sub(transient);
        limits.max_backtrack_bytes = limits.max_backtrack_bytes.min(limits.max_memory_bytes);
        let Some(rec) = self.instr_rec.as_ref() else {
            return limits;
        };
        if rec.remaining != i64::MAX {
            limits.max_steps = limits.max_steps.min(rec.remaining.max(0) as u64);
        }
        if rec.heap_limit != usize::MAX {
            // The cheap resident figure, not the audit walk: this runs on
            // EVERY RegExp exec, and `heap_bytes()` walks the whole heap. A
            // sticky `split` over a 400 KB string performs ~450,000 execs,
            // which the walk turned into 208 s in the browser build (44 ms
            // natively). `heap_bytes_estimate` is the heap's O(1) figure plus
            // the non-heap remainder the last exact audit cached, so between
            // audits it equals the exact total for everything but heap
            // objects that grew in place -- which the strided preflight
            // audit and every host-boundary read reconcile.
            let headroom = rec
                .heap_limit
                .saturating_sub(self.heap_bytes_estimate())
                .saturating_sub(transient);
            limits.max_memory_bytes = limits.max_memory_bytes.min(headroom);
            limits.max_backtrack_bytes = limits.max_backtrack_bytes.min(limits.max_memory_bytes);
        }
        limits
    }

    /// Replacement retains every match before invoking a functional replacer.
    /// Split the same transient/headroom ceiling between executor-owned state
    /// (including capture buffers) and the outer `Vec<Match>`, so the two Rust
    /// allocation families cannot each consume the full allowance.
    #[cfg(feature = "safe-sandbox")]
    pub(crate) fn instrument_regex_collection_limits(&self) -> (regress::MatchLimits, usize) {
        let mut limits = self.instrument_regex_limits();
        let output_bytes = limits.max_memory_bytes / 2;
        limits.max_memory_bytes -= output_bytes;
        limits.max_backtrack_bytes = limits.max_backtrack_bytes.min(limits.max_memory_bytes);
        (limits, output_bytes)
    }

    #[cfg(feature = "safe-sandbox")]
    fn instrument_charge_regex_transient(&mut self, bytes: usize) -> Tick {
        use std::sync::atomic::Ordering;

        if let Some(message) = self
            .instr_rec
            .as_ref()
            .and_then(|rec| rec.terminal_message())
        {
            return Err(message);
        }

        let current = self.regex_transient_bytes.load(Ordering::Relaxed);
        let fixed_available = regress::MatchLimits::SANDBOX
            .max_memory_bytes
            .saturating_sub(current);
        let heap_limit = self
            .instr_rec
            .as_ref()
            .map_or(usize::MAX, |rec| rec.heap_limit);
        let heap_available = if heap_limit == usize::MAX {
            usize::MAX
        } else {
            // Per-exec path too (see `instrument_regex_limits`): the estimate.
            heap_limit
                .saturating_sub(self.heap_bytes_estimate())
                .saturating_sub(current)
        };
        if bytes > fixed_available.min(heap_available) {
            return Err(self.instrument_regex_memory_exhausted());
        }

        // `bytes <= usize::MAX - current` follows from the fixed-allowance
        // comparison above (the fixed allowance is at most 16 MiB).
        self.regex_transient_bytes
            .fetch_add(bytes, Ordering::Relaxed);
        Ok(())
    }

    /// Reserve completed native match storage before any coercion or
    /// functional replacer can call back into RegExp. The allocation already
    /// obeyed the per-collection split limit; this second, scoped accounting is
    /// what makes that allocation visible to nested searches.
    #[cfg(feature = "safe-sandbox")]
    pub(crate) fn instrument_reserve_regex_transient(
        &mut self,
        bytes: usize,
    ) -> Result<RegexTransientReservation, &'static str> {
        let counter = std::sync::Arc::clone(&self.regex_transient_bytes);
        self.instrument_charge_regex_transient(bytes)?;
        Ok(RegexTransientReservation { counter, bytes })
    }

    /// Extend a scoped reservation when a retained result Vec grows between
    /// observable `exec` calls. Capacity growth is charged before the next
    /// guest re-entry; the guard releases the aggregate allocation at exit.
    #[cfg(feature = "safe-sandbox")]
    pub(crate) fn instrument_grow_regex_transient(
        &mut self,
        reservation: &mut RegexTransientReservation,
        bytes: usize,
    ) -> Tick {
        debug_assert!(std::sync::Arc::ptr_eq(
            &reservation.counter,
            &self.regex_transient_bytes
        ));
        self.instrument_charge_regex_transient(bytes)?;
        reservation.bytes = reservation
            .bytes
            .checked_add(bytes)
            .expect("RegExp transient reservation is capped below usize::MAX");
        Ok(())
    }

    /// Return an unused part of a provisional allocation charge. Callers use
    /// this after `try_reserve_exact`/string construction to reconcile the
    /// conservative preflight with the allocator's actual retained capacity.
    #[cfg(feature = "safe-sandbox")]
    pub(crate) fn instrument_shrink_regex_transient(
        &mut self,
        reservation: &mut RegexTransientReservation,
        bytes: usize,
    ) {
        use std::sync::atomic::Ordering;

        debug_assert!(std::sync::Arc::ptr_eq(
            &reservation.counter,
            &self.regex_transient_bytes
        ));
        assert!(
            bytes <= reservation.bytes,
            "RegExp transient-memory reservation shrink underflow"
        );
        reservation.bytes -= bytes;
        let previous = reservation.counter.fetch_sub(bytes, Ordering::Relaxed);
        assert!(
            previous >= bytes,
            "RegExp transient-memory counter shrink underflow"
        );
    }

    #[cfg(feature = "safe-sandbox")]
    pub(crate) fn instrument_regex_memory_exhausted(&mut self) -> &'static str {
        let cause = ResourceExhaustion::RegexMemory;
        match self.instr_rec.as_mut() {
            Some(rec) => rec.exhaust(cause),
            None => cause.message(),
        }
    }

    /// Reconcile completed regex work with the VM recorder before any match
    /// result becomes observable. A regex-local ceiling is a sticky host
    /// resource failure, not a catchable JS exception that can be ignored.
    #[cfg(feature = "safe-sandbox")]
    pub(crate) fn instrument_regex_usage(&mut self, usage: regress::MatchUsage) -> Tick {
        self.charge_steps(usage.steps.min(i64::MAX as u64) as i64);
        let cause = match usage.exhaustion {
            None => return Ok(()),
            Some(regress::MatchLimitError::Steps) => ResourceExhaustion::RegexSteps,
            Some(regress::MatchLimitError::BacktrackMemory) => ResourceExhaustion::RegexMemory,
        };
        if cause == ResourceExhaustion::RegexMemory {
            Err(self.instrument_regex_memory_exhausted())
        } else {
            match self.instr_rec.as_mut() {
                Some(rec) => Err(rec.exhaust(cause)),
                None => Err(cause.message()),
            }
        }
    }

    /// Reject a known contiguous allocation before asking the global allocator
    /// for it. The periodic full-heap scan remains the backstop for aggregate
    /// growth, while this closes the single-allocation overshoot that otherwise
    /// lets an ArrayBuffer exceed a small budget by gigabytes before the next
    /// bytecode poll.
    pub(crate) fn instrument_preflight_heap_growth(&mut self, bytes: usize) -> Tick {
        // Recorder check FIRST: `heap_bytes()` below is a full O(heap-slots)
        // audit walk, and this preflight sits on per-part/per-result string
        // paths (join, builders, regex results). An un-instrumented run — the
        // ordinary `zipp js`, which the CLI now builds WITH this feature for
        // the sandbox command's sake — must not pay a heap walk per append:
        // that measured as markdown-render 0.3s -> 258s+ (worsening as the
        // slot table grows) because the walk ran before this early return.
        let Some(rec) = self.instr_rec.as_ref() else {
            return Ok(());
        };
        if let Some(message) = rec.terminal_message() {
            return Err(message);
        }
        // The walk is priced only when a finite ceiling actually consumes it.
        if rec.heap_limit == usize::MAX {
            return Ok(());
        }
        let heap_limit = rec.heap_limit;

        // The early returns above spare an UNinstrumented run the walk. They do
        // nothing for the hardened profile, which always attaches a recorder and
        // always sets a finite ceiling (`zipp-wasm` calls `set_heap_limit` on
        // every `Engine`), so the artifact this crate exists for paid the full
        // O(heap-slots) audit on every part append. That is the same shape as the
        // 0.3s -> 258s+ regression described above, just reached down a different
        // path: `JSON.stringify` over 16k small objects measured 165us per call
        // against 0.2us to allocate the object being serialised, and the per-call
        // figure doubled every time the loop count doubled.
        //
        // `Heap::resident_bytes` is the O(1) high-water estimate, and
        // `audit_heap_bytes` can only ever RAISE it — the audit reconciles
        // in-place capacity growth into the same monotonic peak and returns
        // `resident_bytes()`. Two consequences, and the asymmetry between them is
        // what makes this safe:
        //
        //   * If the cheap figure already convicts, the exact figure convicts too.
        //     Rejecting on it is not an estimate-based kill; it is the same
        //     verdict reached without paying for it.
        //   * If the cheap figure acquits, the exact one might not, because it can
        //     still discover capacity growth inside objects already counted. That
        //     is the only direction needing the walk, so it rides a stride — the
        //     bargain `instrument_resource_limit_error` already strikes at the
        //     boundary and the interpreter already strikes on `HEAP_AUDIT_MASK`
        //     ticks. The drift window is bounded by the stride, not by the
        //     workload.
        let estimate = self.instrument_heap_estimate();
        if bytes > heap_limit.saturating_sub(estimate) {
            let rec = self
                .instr_rec
                .as_mut()
                .expect("recorder checked present above");
            return Err(rec.exhaust(ResourceExhaustion::Heap));
        }

        let rec = self
            .instr_rec
            .as_mut()
            .expect("recorder checked present above");
        rec.preflight_since_audit = rec.preflight_since_audit.saturating_add(1);
        if rec.preflight_since_audit < PREFLIGHT_AUDIT_STRIDE {
            return Ok(());
        }
        rec.preflight_since_audit = 0;

        let resident = self.heap_bytes();
        let rec = self
            .instr_rec
            .as_mut()
            .expect("recorder checked present above");
        if bytes > heap_limit.saturating_sub(resident) {
            return Err(rec.exhaust(ResourceExhaustion::Heap));
        }
        Ok(())
    }

    /// Mark retained growth outside `HeapObj` payloads. The cheap heap estimate
    /// cannot observe these VM-owned tables, so the next scheduled interpreter
    /// check or explicit host-boundary status read must reconcile exactly.
    /// Repeated mutations coalesce into one walk.
    #[inline]
    pub(crate) fn instrument_mark_external_heap_growth(&mut self) {
        if let Some(rec) = self.instr_rec.as_mut() {
            rec.external_heap_dirty = true;
        }
    }

    /// Exact counterpart for rare allocations whose retained bytes live
    /// outside `HeapObj` payloads and therefore cannot feed the heap's O(1)
    /// high-water estimate when admitted. Compiled RegExp programs are the
    /// motivating case: construction is already substantially more expensive
    /// than this walk, while auditing here makes a sequence of distinct large
    /// programs respect the aggregate ceiling before the next one is retained.
    pub(crate) fn instrument_preflight_external_heap_growth(&mut self, bytes: usize) -> Tick {
        let Some(rec) = self.instr_rec.as_ref() else {
            return Ok(());
        };
        if let Some(message) = rec.terminal_message() {
            return Err(message);
        }
        if rec.heap_limit == usize::MAX {
            return Ok(());
        }
        let heap_limit = rec.heap_limit;
        let resident = self.audit_heap_bytes();
        let rec = self
            .instr_rec
            .as_mut()
            .expect("recorder checked present above");
        rec.preflight_since_audit = 0;
        rec.external_heap_dirty = false;
        if bytes > heap_limit.saturating_sub(resident) {
            return Err(rec.exhaust(ResourceExhaustion::Heap));
        }
        Ok(())
    }

    /// Configure VM-wide dynamic-code ceilings. Every `do_eval` caller is
    /// covered: direct/indirect `eval`, Function constructors, ShadowRealm,
    /// and embedder eval helpers. Requires an attached recorder.
    pub(crate) fn set_dynamic_code_limits(
        &mut self,
        per_source_bytes: usize,
        lifetime_source_bytes: usize,
        calls: usize,
        functions: usize,
        classes: usize,
    ) {
        if let Some(rec) = self.instr_rec.as_mut() {
            rec.set_dynamic_code_limits(
                per_source_bytes,
                lifetime_source_bytes,
                calls,
                functions,
                classes,
            );
        }
    }

    /// Charge one dynamic-code attempt before parsing begins.
    pub(crate) fn instrument_dynamic_code_attempt(&mut self, source_bytes: usize) -> Tick {
        match self.instr_rec.as_mut() {
            Some(rec) => rec.charge_dynamic_code_attempt(source_bytes),
            None => Ok(()),
        }
    }

    /// Preflight the concrete stable-address function/class allocations before
    /// the eval program is installed (and therefore before any `Box::leak`).
    pub(crate) fn instrument_dynamic_code_install(
        &mut self,
        new_functions: usize,
        new_classes: usize,
    ) -> Tick {
        let installed_functions = self.eval_funcs.len();
        let installed_classes = self.eval_classes.len();
        match self.instr_rec.as_mut() {
            Some(rec) => rec.admit_dynamic_code_install(
                installed_functions,
                new_functions,
                installed_classes,
                new_classes,
            ),
            None => Ok(()),
        }
    }

    /// Return the recorder's typed terminal status, first turning a heap
    /// overshoot that happened outside the bytecode loop (for example during a
    /// host-to-guest write) into the same sticky status as an in-loop check.
    /// Postflight for one host re-entry: the ceiling check every crossing of
    /// the embedding boundary pays for.
    ///
    /// This used to open with an unconditional `audit_heap_bytes()`, which walks
    /// every slot in `Heap::objs` — including the slots of objects long since
    /// freed, because the slot table only ever grows. On an embedding that
    /// re-enters constantly, which is the one this crate exists for, that turned
    /// a periodic reconciliation into a per-call tax proportional to everything
    /// the script had ever allocated: measured at 0.8us per round-trip against
    /// 2,478us once 200k objects were live, and it did not come back down when
    /// they were freed. The interpreter had it right — it reconciles once per
    /// `HEAP_AUDIT_MASK` ticks precisely because the walk is expensive.
    ///
    /// `Heap::resident_bytes` answers the same question in O(1) from the
    /// monotonic payload high-water mark. Allocation charges into that mark
    /// eagerly (see `alloc_settled`), so nothing a script or a host write
    /// allocates is invisible here. What it lags is capacity growing *inside* an
    /// object already counted, which is exactly what an audit reconciles — so
    /// the walk still happens, on a stride, and immediately whenever the cheap
    /// figure alone is enough to convict.
    pub(crate) fn instrument_resource_limit_error(&mut self) -> Option<&'static str> {
        let ceiling = match self.instr_rec.as_ref() {
            Some(rec) if rec.exhaustion.is_none() => rec.heap_limit,
            // No recorder, or already exhausted: nothing compares against a heap
            // figure, so nothing should be spent computing one.
            _ => usize::MAX,
        };

        let heap_bytes = if ceiling == usize::MAX {
            None
        } else {
            let reconcile = match self.instr_rec.as_mut() {
                Some(rec) => {
                    rec.postflight_since_audit = rec.postflight_since_audit.saturating_add(1);
                    let stride = rec.postflight_since_audit >= POSTFLIGHT_AUDIT_STRIDE;
                    if stride {
                        rec.postflight_since_audit = 0;
                    }
                    let external = rec.external_heap_dirty;
                    rec.external_heap_dirty = false;
                    stride || external
                }
                None => false,
            };
            // Convict on the cheap figure, then confirm with the exact one: the
            // estimate holds a freed-memory overshoot the audit is what settles,
            // and killing a VM is not a thing to do on an estimate.
            if reconcile || self.instrument_heap_estimate() > ceiling {
                Some(self.audit_heap_bytes())
            } else {
                None
            }
        };

        let rec = self.instr_rec.as_mut()?;
        if rec.exhaustion.is_none() && rec.output_exhausted {
            rec.exhaust(ResourceExhaustion::Output);
        }
        if let Some(bytes) = heap_bytes {
            if rec.exhaustion.is_none() && rec.heap_limit != usize::MAX && bytes > rec.heap_limit {
                rec.exhaust(ResourceExhaustion::Heap);
            }
        }
        rec.terminal_message()
    }

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
    #[cfg(any(test, all(feature = "jit", target_arch = "x86_64")))]
    #[must_use]
    pub(crate) fn meter_lend(&mut self) -> i64 {
        let Some(rec) = self.instr_rec.as_mut() else {
            return 0;
        };
        // Whole-function native entry happens before the interpreter's
        // `instrument_step` hook. Refuse the loan here as well, or a caught
        // dynamic/output failure from a prior entry could execute another
        // compiled function before the sticky status was observed.
        if rec.terminal_message().is_some() {
            self.jit_steps = 0;
            return 0;
        }
        // Once per native entry is the right cadence for the abort poll: the
        // interpreter's own every-4096-instructions check barely advances while
        // a hot loop is running natively.
        #[cfg(not(feature = "meter-only"))]
        {
            if let Some(flag) = rec.abort.as_ref() {
                if flag.load(std::sync::atomic::Ordering::Relaxed) {
                    rec.exhaust(ResourceExhaustion::Abort);
                    self.jit_steps = 0;
                    return 0;
                }
            }
        }
        // The heap ceiling needs checking here for the same reason: compiled
        // code never reaches `instrument_step`, so a native loop that allocates
        // would run to the end of its lent chunk unexamined.
        let ceiling = rec.heap_limit;
        if ceiling != usize::MAX {
            if self.audit_heap_bytes() > ceiling {
                if let Some(rec) = self.instr_rec.as_mut() {
                    rec.exhaust(ResourceExhaustion::Heap);
                }
                self.jit_steps = 0;
                return 0;
            }
            // `heap_bytes` took `&self`, so the recorder has to be re-borrowed.
            let Some(rec) = self.instr_rec.as_mut() else {
                return 0;
            };
            if rec.remaining != i64::MAX && rec.remaining <= 0 {
                rec.exhaust(ResourceExhaustion::Steps);
                self.jit_steps = 0;
                return 0;
            }
            if rec.remaining == i64::MAX {
                rec.used = rec.used.wrapping_add(NATIVE_CHUNK as u64);
                self.jit_steps = NATIVE_CHUNK;
                return NATIVE_CHUNK;
            }
            let lend = rec.remaining.min(NATIVE_CHUNK).max(0);
            rec.remaining -= lend;
            rec.used = rec.used.wrapping_add(lend as u64);
            self.jit_steps = lend;
            return lend;
        }
        if rec.remaining != i64::MAX && rec.remaining <= 0 {
            rec.exhaust(ResourceExhaustion::Steps);
            self.jit_steps = 0;
            return 0;
        }
        if rec.remaining == i64::MAX {
            rec.used = rec.used.wrapping_add(NATIVE_CHUNK as u64);
            self.jit_steps = NATIVE_CHUNK;
            return NATIVE_CHUNK;
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
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn meter_return(&mut self) {
        let unspent = self.jit_steps.max(0);
        if let Some(rec) = self.instr_rec.as_mut() {
            if rec.remaining != i64::MAX {
                rec.remaining += unspent;
                // Zero means native code consumed its final permitted block
                // exactly. Only a negative counter says it attempted to enter a
                // block that did not fit and took the metering exit.
                if self.jit_steps < 0 {
                    rec.exhaust(ResourceExhaustion::Steps);
                }
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
    /// Rust outside `run_loop`, and `FinalizeObject` charges its per-field
    /// remainder here, so nothing else would see either.
    ///
    /// Charge only AFTER the work is known to have completed: a path that
    /// declines half way falls back to a real call, which the interpreter
    /// charges itself, and charging both would be double-counting.
    pub(crate) fn charge_steps(&mut self, n: i64) {
        if let Some(rec) = self.instr_rec.as_mut() {
            let n = n.max(0);
            if rec.remaining != i64::MAX {
                let before = rec.remaining;
                rec.remaining = rec.remaining.saturating_sub(n);
                // The work has already completed. Exactly consuming the final
                // allowance is valid; only a strict overshoot is exhaustion.
                if n > before {
                    rec.exhaust(ResourceExhaustion::Steps);
                }
            }
            #[cfg(all(feature = "meter-only", not(feature = "jit"), not(test)))]
            if rec.remaining == i64::MAX {
                rec.used = rec.used.wrapping_add(n as u64);
            }
            #[cfg(not(all(feature = "meter-only", not(feature = "jit"), not(test))))]
            {
                rec.used = rec.used.wrapping_add(n as u64);
            }
        }
    }

    /// Whether the last native run stopped because it ran out of lent steps
    /// rather than because a type guard failed. A metering exit says nothing
    /// about the region's quality, so it must not count toward eviction.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn meter_exhausted(&self) -> bool {
        self.instr_rec.is_some() && self.jit_steps < 0
    }

    /// The displacement of [`Self::jit_steps`] from the VM pointer, for the
    /// compilers to bake into `[rdi + off]`. `None` leaves the compiled code
    /// byte-identical to an unmetered build.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn meter_offset(&self) -> Option<i32> {
        self.instr_rec.as_ref()?;
        let base = self as *const Self as usize;
        let field = &self.jit_steps as *const i64 as usize;
        Some((field - base) as i32)
    }
}

impl super::Vm<'_> {
    /// Charge one buffered console line before retaining it. Once exhausted,
    /// the next instruction also fails so a script cannot catch and continue.
    pub(crate) fn instrument_output_line(&mut self, line: &str) -> Tick {
        let Some(rec) = self.instr_rec.as_mut() else {
            return Ok(());
        };
        let bytes = line.len().saturating_add(OUTPUT_LINE_OVERHEAD_BYTES);
        let Some(total) = rec.output_used.checked_add(bytes) else {
            rec.output_exhausted = true;
            return Err(rec.exhaust(ResourceExhaustion::Output));
        };
        if total > rec.output_limit {
            rec.output_exhausted = true;
            return Err(rec.exhaust(ResourceExhaustion::Output));
        }
        rec.output_used = total;
        Ok(())
    }

    /// The dispatch loop's hook, run once per instruction *before* it executes:
    /// completes the previous row, charges one step against the budget, polls
    /// the abort flag, and opens a row for `instr`.
    #[cfg(all(feature = "meter-only", not(feature = "jit"), not(test)))]
    #[inline(always)]
    pub(crate) fn instrument_step(&mut self, _base: usize, _ip: usize, _instr: &Instr) -> Tick {
        let rec = match self.instr_rec.as_mut() {
            Some(rec) => rec,
            None => return Ok(()),
        };
        if let Some(message) = rec.terminal_message() {
            return Err(message);
        }
        // `meter-only` is the zipp-wasm artifact profile. Its deadline is an
        // external Worker termination and it never installs the cooperative
        // abort flag. Output exhaustion is already sticky in `exhaustion` at
        // the write that crosses the limit, so neither state needs a second
        // per-instruction probe here.
        let heap_poll = if rec.remaining != i64::MAX {
            if rec.remaining <= 0 {
                return Err(rec.exhaust(ResourceExhaustion::Steps));
            }
            rec.remaining -= 1;
            let balance = rec.remaining as u64;
            if balance <= rec.used {
                // Off-loop work can cross a threshold between dispatches. Poll
                // on the next dispatch and schedule from the current balance,
                // bounding the next interval by 65,536 additional metered
                // steps without maintaining a per-dispatch counter.
                rec.used = finite_next_heap_poll_balance(balance);
                true
            } else {
                false
            }
        } else {
            // With no finite starting balance there is no countdown from which
            // to recover the dispatch clock, so retain the real counter.
            rec.ticks = rec.ticks.wrapping_add(1);
            rec.ticks & HEAP_AUDIT_MASK == 0
        };

        if heap_poll {
            self.instrument_heap_poll()?;
        }
        Ok(())
    }

    #[cfg(all(feature = "meter-only", any(feature = "jit", test)))]
    #[inline(always)]
    pub(crate) fn instrument_step(&mut self, _base: usize, _ip: usize, _instr: &Instr) -> Tick {
        let rec = match self.instr_rec.as_mut() {
            Some(rec) => rec,
            None => return Ok(()),
        };
        if let Some(message) = rec.terminal_message() {
            return Err(message);
        }
        if rec.remaining != i64::MAX {
            if rec.remaining <= 0 {
                return Err(rec.exhaust(ResourceExhaustion::Steps));
            }
            rec.remaining -= 1;
        }
        rec.ticks = rec.ticks.wrapping_add(1);
        let heap_poll = rec.ticks & HEAP_AUDIT_MASK == 0;

        if heap_poll {
            self.instrument_heap_poll()?;
        }
        Ok(())
    }

    #[cfg(not(feature = "meter-only"))]
    #[inline]
    pub(crate) fn instrument_step(&mut self, base: usize, ip: usize, instr: &Instr) -> Tick {
        let rec = match self.instr_rec.as_mut() {
            Some(rec) => rec,
            None => return Ok(()),
        };

        // The production wasm engine meters without tracing or an abort flag.
        // Keep that overwhelmingly common path small enough to live beside the
        // opcode dispatch; trace completion, atomics, and row construction stay
        // in the out-of-line path below. `pending` is checked independently so
        // ending a trace can never strand its final row.
        if rec.tracing || rec.pending.is_some() || rec.abort.is_some() {
            return self.instrument_step_slow(base, ip, instr);
        }
        if let Some(message) = rec.terminal_message() {
            return Err(message);
        }
        if rec.output_exhausted {
            return Err(rec.exhaust(ResourceExhaustion::Output));
        }
        if rec.remaining != i64::MAX {
            if rec.remaining <= 0 {
                return Err(rec.exhaust(ResourceExhaustion::Steps));
            }
            rec.remaining -= 1;
        }
        rec.ticks = rec.ticks.wrapping_add(1);
        let heap_poll = rec.ticks & HEAP_AUDIT_MASK == 0;

        if heap_poll {
            self.instrument_heap_poll()?;
        }
        Ok(())
    }

    /// Trace/abort-enabled metering. Kept out of the production wasm dispatch
    /// loop so V8 sees a compact common case instead of the complete prover and
    /// heap-audit machinery at every opcode.
    #[cfg(not(feature = "meter-only"))]
    #[cold]
    #[inline(never)]
    fn instrument_step_slow(&mut self, base: usize, ip: usize, instr: &Instr) -> Tick {
        // Completing the previous row needs `&self.regs`, so it cannot happen
        // inside the `&mut self.instr_rec` borrow below.
        self.instrument_complete();

        let rec = match self.instr_rec.as_mut() {
            Some(r) => r,
            None => return Ok(()),
        };
        if let Some(message) = rec.terminal_message() {
            return Err(message);
        }
        if rec.output_exhausted {
            return Err(rec.exhaust(ResourceExhaustion::Output));
        }
        if rec.remaining != i64::MAX {
            if rec.remaining <= 0 {
                return Err(rec.exhaust(ResourceExhaustion::Steps));
            }
            rec.remaining -= 1;
        }
        // `ticks` is both the polling clock and the interpreter-work counter.
        // Advance it only after the budget check: an instruction rejected at
        // zero remaining steps did not execute and must not be billed.
        rec.ticks = rec.ticks.wrapping_add(1);
        if rec.ticks & ABORT_CHECK_MASK == 0 {
            if let Some(flag) = rec.abort.as_ref() {
                if flag.load(std::sync::atomic::Ordering::Relaxed) {
                    return Err(rec.exhaust(ResourceExhaustion::Abort));
                }
            }
        }
        let heap_poll = rec.ticks & HEAP_AUDIT_MASK == 0;
        let tracing = rec.tracing;

        if heap_poll {
            self.instrument_heap_poll()?;
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

    /// Periodic heap-limit reconciliation shared by ordinary metering and the
    /// trace/abort path. Ordinary meters call it every 65,536 dispatches; a
    /// finite release-wasm meter calls it on the first dispatch after its next
    /// 65,536-step countdown threshold. The expensive work remains off the
    /// common instruction edge.
    ///
    /// `Vm::gc_from_poll` also calls it after a collection. That caller is
    /// scheduled on BYTES rather than instructions, which is what bounds the
    /// overshoot of a script whose single instructions allocate megabytes; see
    /// the comment there for why the tick stride alone is the wrong unit.
    #[cold]
    #[inline(never)]
    pub(crate) fn instrument_heap_poll(&mut self) -> Tick {
        self.instrument_heap_poll_inner(true)
    }

    /// The same reconciliation, driven by a collection rather than by the
    /// instruction stride.
    ///
    /// `advance_walk_stride: false` is the whole difference, and it is what
    /// keeps this caller free. `ticks_since_heap_walk` schedules the O(heap
    /// slots) walk once per `HEAP_WALK_STRIDE` polls; it counts POLLS, and its
    /// budget was sized for polls that arrive every 65,536 instructions.
    /// Letting a GC-driven poll advance it too would make the walk fire far
    /// more often in exactly the allocation-heavy code that collects most —
    /// the 3.3x-to-7.2x tax the comment above warns about.
    ///
    /// Not advancing it costs nothing here, because fresh allocations are
    /// charged eagerly into the O(1) estimate: an over-ceiling script is
    /// convicted by the cheap figure and only then pays one confirming walk.
    /// The one case the cheap figure cannot see is a heap whose payload
    /// accounting has never been switched on — it is enabled lazily, by the
    /// first walk — so force a reconcile while that is still true.
    #[cold]
    #[inline(never)]
    pub(crate) fn instrument_heap_poll_after_gc(&mut self) -> Tick {
        let force = !self.heap.payload_accounting_enabled();
        self.instrument_heap_poll_inner(force)
    }

    #[cold]
    #[inline(never)]
    fn instrument_heap_poll_inner(&mut self, advance_walk_stride: bool) -> Tick {
        let ceiling = match self.instr_rec.as_ref() {
            Some(rec) => rec.heap_limit,
            None => return Ok(()),
        };

        // Escalate, then confirm — the same change commit 935f6dd made at the
        // host boundary, which the interpreter's own poll was left out of.
        //
        // `audit_heap_bytes` is O(heap slots) and the slot table only grows, so
        // running it on a fixed instruction stride costs an app a tax
        // proportional to everything it is holding, for code that allocates
        // nothing: measured at 3.3x on a non-allocating integer loop with 150,000
        // live objects and 7.2x at 300,000. Only the wasm build ever felt it,
        // because `heap_limit` defaults to `usize::MAX` and `zipp-wasm` is the
        // one embedding that always calls `set_heap_limit` — which is also why
        // no native benchmark could see it.
        //
        // `Heap::resident_bytes` is O(1) and the audit can only ever raise it
        // (the walk reconciles in-place growth into the same monotonic peak and
        // returns `resident_bytes()`). So headroom on the cheap figure is
        // headroom the walk cannot take away, and the walk is only needed to
        // reconcile capacity growth inside objects already counted — which rides
        // a stride of its own rather than every poll.
        if ceiling == usize::MAX {
            return Ok(());
        }
        // Growing the per-slot tables needs them twice over for the copy, and
        // a WebAssembly host cannot supply memory past the ceiling for that:
        // charge the next growth now, so the guest is convicted while the
        // request is still affordable rather than trapped when it is not.
        let growth = self.heap.slot_table_growth_reserve();
        let convicted = self.instrument_heap_estimate().saturating_add(growth) > ceiling;
        // The walk costs O(slots), which is what the stride protects; a heap
        // of few slots can afford to reconcile every poll. That is exactly
        // the heap the cheap figure cannot see: one object growing its
        // property tables in place to a gigabyte is one slot, charged once
        // at birth, and before this it reached the WebAssembly build's
        // linked memory between two walks.
        let walk_stride = heap_walk_stride_for(self.heap.len());
        let reconcile = match self.instr_rec.as_mut() {
            Some(rec) => {
                let stride = if advance_walk_stride {
                    rec.ticks_since_heap_walk = rec.ticks_since_heap_walk.saturating_add(1);
                    let due = rec.ticks_since_heap_walk >= walk_stride;
                    if due {
                        rec.ticks_since_heap_walk = 0;
                    }
                    due
                } else {
                    false
                };
                let external = rec.external_heap_dirty;
                rec.external_heap_dirty = false;
                stride || external
            }
            None => false,
        };
        if (convicted || reconcile) && self.audit_heap_bytes().saturating_add(growth) > ceiling {
            let rec = self.instr_rec.as_mut().expect("recorder checked above");
            return Err(rec.exhaust(ResourceExhaustion::Heap));
        }
        Ok(())
    }

    /// Re-check the heap ceiling from inside a native drain.
    ///
    /// `Array.from`, spread, the collection constructors and the destructuring
    /// helpers pull an iterator to exhaustion inside ONE instruction, with the
    /// collector suspended for the scope, so every `{value, done}` result they
    /// allocate stays live until the drain returns. The dispatch-stride poll
    /// never runs meanwhile, and the eager-result cap alone permits four
    /// million results -- more than a gigabyte of them -- before it fires. A
    /// WebAssembly host reaches its linked memory first and traps. Polling
    /// every 1,024 steps turns that into the ceiling's catchable RangeError
    /// while the request is still affordable; the check is the O(1) estimate
    /// with the growth reserve, and it never advances the walk stride.
    pub(crate) fn instrument_drain_heap_check(&mut self, steps: usize) -> Result<(), super::Thrown> {
        if steps & 1023 != 0 {
            return Ok(());
        }
        self.instrument_heap_poll_inner(false)
            .map_err(|message| super::Thrown(message.into()))
    }

    /// Fill in the previous row's `val_dst` and confirm — or drop — its claim.
    #[cfg(not(feature = "meter-only"))]
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
    #[cfg(not(feature = "meter-only"))]
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

    #[cfg(not(feature = "meter-only"))]
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
#[cfg(not(feature = "meter-only"))]
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
#[cfg(not(feature = "meter-only"))]
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
#[cfg(not(feature = "meter-only"))]
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
#[cfg(not(feature = "meter-only"))]
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
        // AddRightPair represents TWO ordered Adds and the AIR row has room
        // for only one binary relation. Keep it explicitly OTHER rather than
        // making a false single-Add claim.
        AddRightPair { dst, a, b, .. } => (op::OTHER, Claim::None, Some(a), Some(b), 0, Some(dst)),
        // The left operand is an implicit fixed string literal, so there is no
        // second register with which to make a truthful arithmetic AIR claim.
        Pad2Concat { dst, src, zero } => (
            op::OTHER,
            Claim::None,
            Some(src),
            None,
            zero as u64,
            Some(dst),
        ),
        Pad2Conditional { dst, src } => (op::OTHER, Claim::None, Some(src), None, 0, Some(dst)),
        // The fused op combines an indexed read and an append, so neither the
        // property nor arithmetic claim schema can represent it faithfully.
        StrAppendIndex { dst, a, obj, .. } => {
            (op::OTHER, Claim::None, Some(a), Some(obj), 0, Some(dst))
        }
        // StrConcat/StrAppendInPlace are `Add` with a JIT routing hint; they are
        // the same operator and the post-check decides whether the row's values
        // actually satisfy addition.
        Add { dst, a, b }
        | StrConcat { dst, a, b }
        | StrAppendInPlace { dst, a, b }
        | StrConcatChain { dst, a, b } => (
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
        Call { dst, callee, .. }
        | CallWithThis { dst, callee, .. }
        | RegExpMethod { dst, callee, .. } => {
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
        NewArray { dst, .. }
        | NewObject { dst, .. }
        | NewPlannedObject { dst, .. }
        | NewMap { dst, .. }
        | NewSet { dst, .. } => (op::COLLECTION, Claim::Flat, None, None, 0, Some(dst)),

        // Everything else attests only that a step happened here.
        _ => (op::OTHER, Claim::None, None, None, 0, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(all(feature = "meter-only", feature = "jit", target_arch = "x86_64"))]
    use crate::value::Value;

    fn program(src: &str) -> &'static crate::bytecode::Program {
        let ast = crate::front::parse_script(src).expect("source parses");
        Box::leak(Box::new(
            crate::compile::compile_program(&ast, src).expect("source compiles"),
        ))
    }

    fn instrumented_vm(src: &str) -> crate::vm::Vm<'static> {
        let mut vm = crate::vm::Vm::new(program(src));
        let mut rec = Recorder::new();
        rec.remaining = 1_000_000;
        vm.set_instrumentation(rec);
        vm
    }

    #[cfg(feature = "safe-sandbox")]
    #[test]
    fn regex_transient_reservations_are_nested_and_scoped() {
        fn return_early(vm: &mut crate::vm::Vm<'_>, bytes: usize) -> Tick {
            let _reservation = vm.instrument_reserve_regex_transient(bytes)?;
            Err("sentinel")
        }

        let mut vm = instrumented_vm("");
        let full = regress::MatchLimits::SANDBOX.max_memory_bytes;
        let first_bytes = 64 * 1024;
        let second_bytes = 32 * 1024;
        let first = vm
            .instrument_reserve_regex_transient(first_bytes)
            .expect("first reservation fits");
        assert_eq!(
            vm.instrument_regex_limits().max_memory_bytes,
            full - first_bytes
        );
        {
            let _second = vm
                .instrument_reserve_regex_transient(second_bytes)
                .expect("nested reservation fits");
            assert_eq!(
                vm.instrument_regex_limits().max_memory_bytes,
                full - first_bytes - second_bytes
            );
        }
        assert_eq!(
            vm.instrument_regex_limits().max_memory_bytes,
            full - first_bytes
        );
        drop(first);
        assert_eq!(vm.instrument_regex_limits().max_memory_bytes, full);

        assert_eq!(return_early(&mut vm, first_bytes), Err("sentinel"));
        assert_eq!(
            vm.instrument_regex_limits().max_memory_bytes,
            full,
            "an early return must drop and release the reservation"
        );
    }

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
    #[cfg(not(feature = "meter-only"))]
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

    #[cfg(not(feature = "meter-only"))]
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

    #[cfg(feature = "meter-only")]
    #[test]
    fn finite_countdown_accounts_for_weighted_threshold_crossings() {
        let initial = 200_000_u64;
        let first_threshold = finite_next_heap_poll_balance(initial);
        let balance_before_weighted_charge = first_threshold + 1;
        assert!(balance_before_weighted_charge > first_threshold);

        // A weighted operation may jump across the exact threshold between
        // dispatch hooks. The following dispatch still observes `<=`, polls,
        // and schedules a full interval from the balance it actually saw.
        let balance_at_next_dispatch = balance_before_weighted_charge - 37 - 1;
        assert!(balance_at_next_dispatch <= first_threshold);
        let second_threshold = finite_next_heap_poll_balance(balance_at_next_dispatch);
        assert_eq!(
            balance_at_next_dispatch - second_threshold,
            HEAP_AUDIT_INTERVAL
        );

        assert_eq!(
            finite_meter_total_used(initial, balance_at_next_dispatch as i64),
            initial - balance_at_next_dispatch,
            "the public total includes both dispatch and off-loop countdown charges"
        );

        // A final partial interval polls at zero and never wraps its threshold.
        let partial_balance = HEAP_AUDIT_INTERVAL - 9;
        let final_threshold = finite_next_heap_poll_balance(partial_balance);
        assert_eq!(final_threshold, 0);
        let poll_due = |balance| balance <= final_threshold;
        assert!(!poll_due(1), "a positive final balance is not due yet");
        assert!(poll_due(0), "the final permitted step is due");

        assert_eq!(
            finite_meter_total_used(initial, -9),
            initial,
            "an overshoot must saturate rather than wrapping the billed total"
        );
        assert_eq!(finite_meter_total_used(initial, initial as i64 + 1), 0);
    }

    #[test]
    fn exact_final_step_is_success_not_exhaustion() {
        let src = "var answer = 42;";

        let mut measured = crate::vm::Vm::new(program(src));
        measured.set_instrumentation(Recorder::new());
        measured.run().expect("measurement run succeeds");
        let used = measured
            .instr_rec
            .as_ref()
            .expect("recorder attached")
            .steps_used();
        assert!(used > 0);

        let mut exact = crate::vm::Vm::new(program(src));
        let mut exact_rec = Recorder::new();
        exact_rec.remaining = used as i64;
        exact.set_instrumentation(exact_rec);
        exact
            .run()
            .expect("the final permitted instruction may halt");
        assert_eq!(exact.instr_rec.as_ref().unwrap().remaining, 0);
        assert_eq!(exact.instrument_resource_limit_error(), None);

        let mut short = crate::vm::Vm::new(program(src));
        let mut short_rec = Recorder::new();
        short_rec.remaining = (used - 1) as i64;
        short.set_instrumentation(short_rec);
        short.run().expect_err("one fewer step is rejected");
        assert_eq!(
            short.instr_rec.as_ref().unwrap().exhaustion,
            Some(ResourceExhaustion::Steps)
        );
        assert_eq!(short.instrument_resource_limit_error(), Some(BUDGET_MSG));
    }

    /// The meter-only dispatch may forward a successful plain own-data store
    /// into an immediately adjacent read. It is still two bytecodes: exhausting
    /// the budget between them must leave the store committed while rejecting
    /// the read before its destination changes.
    #[cfg(feature = "meter-only")]
    #[test]
    fn adjacent_set_get_forwarding_keeps_the_exact_budget_boundary() {
        const SRC: &str = "var o={x:0}; function pair(v){ let q=o; q.x=v; return q.x; }";
        let program = program(SRC);
        let fid = program
            .functions
            .iter()
            .position(|f| f.name == "pair")
            .expect("pair function exists");
        let pair = &program.functions[fid];
        let set_ip = pair
            .code
            .windows(2)
            .position(|ops| match (&ops[0], &ops[1]) {
                (
                    Instr::SetProp {
                        obj,
                        name,
                        val: _,
                        strict: _,
                    },
                    Instr::GetProp {
                        obj: get_obj,
                        name: get_name,
                        dst: _,
                    },
                ) => {
                    obj == get_obj
                        && pair.string_constants[*name as usize]
                            == pair.string_constants[*get_name as usize]
                }
                _ => false,
            })
            .expect("compiler emits the adjacent same-property pair");
        let executed_len = pair
            .code
            .iter()
            .position(|op| matches!(op, Instr::Return { .. }))
            .map(|ip| ip + 1)
            .expect("pair has an explicit return");
        let pair_slot = pair.name_global.expect("pair has a global slot") as usize;
        let o_slot = program
            .global_names
            .iter()
            .position(|name| name == "o")
            .expect("o has a global slot");

        let mut exact = crate::vm::Vm::new(program);
        exact.run().expect("top level initializes");
        exact.set_instrumentation(Recorder::new());
        let callee = exact.globals[pair_slot];
        assert_eq!(
            exact
                .call_value(
                    callee,
                    crate::value::Value::UNDEFINED,
                    &[crate::value::Value::int(7)],
                )
                .expect("the full pair succeeds"),
            crate::value::Value::int(7)
        );
        assert_eq!(
            exact.instr_rec.as_ref().unwrap().steps_used(),
            executed_len as u64,
            "forwarding must charge every logical bytecode"
        );

        let mut boundary = crate::vm::Vm::new(program);
        boundary.run().expect("top level initializes");
        let mut rec = Recorder::new();
        rec.remaining = (set_ip + 1) as i64;
        boundary.set_instrumentation(rec);
        let callee = boundary.globals[pair_slot];
        let err = boundary
            .call_value(
                callee,
                crate::value::Value::UNDEFINED,
                &[crate::value::Value::int(7)],
            )
            .expect_err("the adjacent GetProp must cross the boundary");
        assert!(err.0.contains("instruction budget"), "got {err:?}");
        let object = boundary.globals[o_slot];
        assert_eq!(
            boundary.get_prop(object, "x").expect("plain own property"),
            crate::value::Value::int(7),
            "the SetProp before the boundary remains committed"
        );
    }

    #[cfg(all(feature = "meter-only", not(feature = "jit")))]
    #[test]
    fn numeric_fib_kernel_preserves_results_binding_guards_and_exact_steps() {
        const SRC: &str = "function fib(n){ return n < 2 ? n : fib(n-1) + fib(n-2); }";
        const N: i32 = 30;

        fn fib_steps(n: i32) -> u64 {
            let (mut a, mut b) = (3u64, 3u64);
            if n < 2 {
                return 3;
            }
            for _ in 2..=n {
                (a, b) = (b, 10 + a + b);
            }
            b
        }

        fn ready(src: &str, remaining: i64) -> crate::vm::Vm<'static> {
            let mut vm = crate::vm::Vm::new(program(src));
            vm.run().expect("top level initializes");
            let mut rec = Recorder::new();
            rec.remaining = remaining;
            vm.set_instrumentation(rec);
            vm
        }

        let expected_steps = fib_steps(N);
        let mut measured = ready(SRC, i64::MAX);
        let slot = measured.func(1).name_global.expect("fib has a global slot");
        let callee = measured.globals[slot as usize];
        assert_eq!(
            measured
                .call_value(
                    callee,
                    crate::value::Value::UNDEFINED,
                    &[crate::value::Value::int(N)],
                )
                .expect("collapsed recursion succeeds"),
            crate::value::Value::int(832_040)
        );
        assert_eq!(
            measured.instr_rec.as_ref().unwrap().steps_used(),
            expected_steps,
            "off-dispatch recursion must retain the historical bytecode bill"
        );

        let mut exact = ready(SRC, expected_steps as i64);
        let callee = exact.globals[slot as usize];
        assert_eq!(
            exact
                .call_value(
                    callee,
                    crate::value::Value::UNDEFINED,
                    &[crate::value::Value::int(N)],
                )
                .expect("the exact final allowance succeeds"),
            crate::value::Value::int(832_040)
        );
        assert_eq!(exact.instr_rec.as_ref().unwrap().remaining, 0);
        assert_eq!(exact.instrument_resource_limit_error(), None);

        let mut short = ready(SRC, expected_steps as i64 - 1);
        let callee = short.globals[slot as usize];
        let err = short
            .call_value(
                callee,
                crate::value::Value::UNDEFINED,
                &[crate::value::Value::int(N)],
            )
            .expect_err("one missing logical bytecode is rejected");
        assert!(err.0.contains("instruction budget"), "got {err:?}");
        assert_eq!(
            short.instr_rec.as_ref().unwrap().exhaustion,
            Some(ResourceExhaustion::Steps)
        );

        const NEAR_MISS: &str = r#"
            function almostFib(n) {
                if (n < 0) return 0;
                return n < 2 ? n : almostFib(n-1) + almostFib(n-2);
            }
        "#;
        const NEAR_N: i32 = 8;
        let mut near = ready(NEAR_MISS, i64::MAX);
        let near_slot = near
            .func(1)
            .name_global
            .expect("near miss has a global slot");
        let near_callee = near.globals[near_slot as usize];
        assert_eq!(
            near.call_value(
                near_callee,
                crate::value::Value::UNDEFINED,
                &[crate::value::Value::int(NEAR_N)],
            )
            .expect("near-match recursion succeeds"),
            crate::value::Value::int(21)
        );
        assert!(
            near.instr_rec.as_ref().unwrap().steps_used() > fib_steps(NEAR_N),
            "a FuncProto with extra observable control flow must fail the exact cached classifier"
        );

        const REBOUND: &str = r#"
            function fib(n){ return n < 2 ? n : fib(n-1) + fib(n-2); }
            var saved = fib;
            fib = function (_) { return 7; };
        "#;
        let mut rebound = ready(REBOUND, i64::MAX);
        let saved_slot = rebound
            .program
            .global_names
            .iter()
            .position(|name| name == "saved")
            .expect("saved has a global slot");
        let saved = rebound.globals[saved_slot];
        assert_eq!(
            rebound
                .call_value(
                    saved,
                    crate::value::Value::UNDEFINED,
                    &[crate::value::Value::int(5)],
                )
                .expect("rebound recursion follows the live global"),
            crate::value::Value::int(14),
            "a changed recursive binding must decline the collapsed kernel"
        );
    }

    #[test]
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    fn exact_final_native_block_succeeds_and_the_next_attempt_fails() {
        const SRC: &str = "function add1(x) { return x + 1; }";

        fn exercise(
            limit: i64,
            interpreter_only: bool,
        ) -> (crate::vm::Vm<'static>, Result<Value, crate::vm::Thrown>) {
            let mut vm = crate::vm::Vm::new(program(SRC));
            let mut rec = Recorder::new();
            rec.remaining = limit;
            vm.set_instrumentation(rec);
            if interpreter_only {
                vm.set_jit_enabled(false);
            }
            vm.run().expect("top level initializes");

            let slot = vm.func(1).name_global.expect("add1 has a global slot");
            let callee = vm.globals[slot as usize];
            for i in 0..8 {
                assert_eq!(
                    vm.call_value(callee, Value::UNDEFINED, &[Value::int(i)])
                        .expect("warm call succeeds"),
                    Value::int(i + 1)
                );
            }
            if !interpreter_only {
                assert!(
                    vm.jit.get(1).is_some(),
                    "the boundary call must exercise compiled code"
                );
            }
            let result = vm.call_value(callee, Value::UNDEFINED, &[Value::int(8)]);
            (vm, result)
        }

        // First measure this deterministic run with an unlimited recorder. The
        // ninth call is the first one to enter the whole-function native body.
        let (measured, result) = exercise(i64::MAX, false);
        assert_eq!(result.expect("measurement call succeeds"), Value::int(9));
        let exact_steps = measured.instr_rec.as_ref().unwrap().steps_used();
        assert!(exact_steps > 0);
        assert!(
            exact_steps < NATIVE_CHUNK as u64,
            "the tier oracle must not conflate block accounting with chunk boundaries"
        );

        let (interpreted, result) = exercise(i64::MAX, true);
        assert_eq!(result.expect("interpreter oracle succeeds"), Value::int(9));
        assert_eq!(
            exact_steps,
            interpreted.instr_rec.as_ref().unwrap().steps_used(),
            "native and interpreter execution must charge the same bytecodes"
        );

        let (mut exact, result) = exercise(exact_steps as i64, false);
        assert_eq!(result.expect("exact native call succeeds"), Value::int(9));
        assert_eq!(exact.instr_rec.as_ref().unwrap().remaining, 0);
        assert_eq!(exact.instrument_resource_limit_error(), None);

        let slot = exact.func(1).name_global.unwrap();
        let callee = exact.globals[slot as usize];
        let err = exact
            .call_value(callee, Value::UNDEFINED, &[Value::int(9)])
            .expect_err("another attempted native instruction must fail");
        assert!(err.0.contains("instruction budget"), "got {err:?}");
        assert_eq!(
            exact.instr_rec.as_ref().unwrap().exhaustion,
            Some(ResourceExhaustion::Steps)
        );

        let (mut short, result) = exercise(exact_steps as i64 - 1, false);
        let err = result.expect_err("one fewer step must reject the native call");
        assert!(err.0.contains("instruction budget"), "got {err:?}");
        assert_eq!(short.instrument_resource_limit_error(), Some(BUDGET_MSG));
    }

    #[cfg(not(feature = "meter-only"))]
    #[test]
    fn abort_has_typed_sticky_status_and_exact_accounting() {
        const ABORT_BUDGET: i64 = 10_000;
        const ABORT_POLL: u64 = ABORT_CHECK_MASK + 1;
        let mut aborted = crate::vm::Vm::new(program("while (true) {}"));
        let mut abort_rec = Recorder::new();
        abort_rec.remaining = ABORT_BUDGET;
        abort_rec.abort = Some(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
            true,
        )));
        aborted.set_instrumentation(abort_rec);
        aborted.run().expect_err("host abort stops execution");
        let abort_rec = aborted.instr_rec.as_ref().unwrap();
        assert_eq!(abort_rec.exhaustion, Some(ResourceExhaustion::Abort));
        let used = abort_rec.steps_used();
        assert!(
            (1..=ABORT_POLL).contains(&used),
            "the interpreter poll or an earlier native-entry poll must stop execution; used {used}"
        );
        assert_eq!(abort_rec.remaining, ABORT_BUDGET - used as i64);
        assert_eq!(used + abort_rec.remaining as u64, ABORT_BUDGET as u64);
        assert_eq!(aborted.instrument_resource_limit_error(), Some(ABORT_MSG));
    }

    #[test]
    fn postflight_heap_has_typed_sticky_status() {
        let mut heap = instrumented_vm("var answer = 42;");
        heap.run()
            .expect("short run need not hit the periodic heap poll");
        heap.instr_rec.as_mut().unwrap().heap_limit = 0;
        assert_eq!(
            heap.instrument_resource_limit_error(),
            Some(MEMORY_MSG),
            "postflight must catch allocations made between periodic polls"
        );
        assert_eq!(
            heap.instr_rec.as_ref().unwrap().exhaustion,
            Some(ResourceExhaustion::Heap)
        );
    }

    #[test]
    fn all_guest_dynamic_compilers_share_the_source_gate() {
        for src in [
            "try { eval('12'); } catch (_) {} var after = 1;",
            "try { Function('return 1'); } catch (_) {} var after = 1;",
            "try { new ShadowRealm().evaluate('12'); } catch (_) {} var after = 1;",
        ] {
            let mut vm = instrumented_vm(src);
            vm.set_dynamic_code_limits(1, 1024, 16, 128, 32);
            vm.run()
                .expect_err("a caught dynamic-source error remains terminal");
            assert_eq!(
                vm.instr_rec.as_ref().unwrap().exhaustion,
                Some(ResourceExhaustion::DynamicSource),
                "source path: {src}"
            );
            assert_eq!(
                vm.instrument_resource_limit_error(),
                Some(DYNAMIC_SOURCE_MSG)
            );
            assert_eq!(
                vm.meter_lend(),
                0,
                "sticky exhaustion must refuse pre-interpreter native entry"
            );
        }
    }

    #[test]
    fn failed_parses_consume_dynamic_call_and_source_allowances() {
        let mut calls = instrumented_vm(
            "try { eval('('); } catch (_) {} try { eval('1'); } catch (_) {} var after = 1;",
        );
        calls.set_dynamic_code_limits(1024, 1024, 1, 128, 32);
        calls
            .run()
            .expect_err("the second attempt exceeds the call allowance");
        assert_eq!(
            calls.instr_rec.as_ref().unwrap().exhaustion,
            Some(ResourceExhaustion::DynamicCalls)
        );

        let mut bytes = instrumented_vm(
            "try { eval('('); } catch (_) {} try { eval('1'); } catch (_) {} var after = 1;",
        );
        bytes.set_dynamic_code_limits(1024, 1, 16, 128, 32);
        bytes
            .run()
            .expect_err("the second byte exceeds the aggregate allowance");
        assert_eq!(
            bytes.instr_rec.as_ref().unwrap().exhaustion,
            Some(ResourceExhaustion::DynamicTotalSource)
        );
    }

    #[test]
    fn function_constructor_precharges_before_standalone_parameter_parse() {
        let mut failed = instrumented_vm(
            "try { Function('('); } catch (_) {} \
             try { Function('('); } catch (_) {} var after = 1;",
        );
        failed.set_dynamic_code_limits(1024, 4096, 1, 128, 32);
        failed
            .run()
            .expect_err("the second malformed parameter list exceeds the call allowance");
        assert_eq!(
            failed.instr_rec.as_ref().unwrap().exhaustion,
            Some(ResourceExhaustion::DynamicCalls)
        );
        assert!(
            failed.eval_funcs.is_empty(),
            "neither malformed source installs code"
        );

        let mut oversized =
            instrumented_vm("try { Function('('.repeat(128)); } catch (_) {} var after = 1;");
        oversized.set_dynamic_code_limits(64, 4096, 16, 128, 32);
        oversized
            .run()
            .expect_err("the complete wrapper is gated before its invalid params are parsed");
        assert_eq!(
            oversized.instr_rec.as_ref().unwrap().exhaustion,
            Some(ResourceExhaustion::DynamicSource)
        );
        assert!(
            oversized.eval_funcs.is_empty(),
            "oversized source installs no code"
        );

        let mut valid = instrumented_vm("var made = Function('return 7'); var answer = made();");
        valid.set_dynamic_code_limits(1024, 4096, 1, 128, 32);
        valid
            .run()
            .expect("one valid constructor is charged exactly once");
        assert_eq!(valid.instr_rec.as_ref().unwrap().dynamic_calls, 1);
        assert_eq!(valid.instrument_resource_limit_error(), None);
    }

    #[test]
    fn concrete_function_and_class_caps_precede_any_leak() {
        let mut functions = instrumented_vm(
            "try { eval('function a(){}; function b(){}'); } catch (_) {} var after = 1;",
        );
        functions.set_dynamic_code_limits(1024, 4096, 16, 2, 32);
        functions
            .run()
            .expect_err("eval body plus declarations exceeds two FuncProtos");
        assert_eq!(
            functions.instr_rec.as_ref().unwrap().exhaustion,
            Some(ResourceExhaustion::DynamicFunctions)
        );
        assert!(
            functions.eval_funcs.is_empty(),
            "rejection must precede leaks"
        );

        let mut classes =
            instrumented_vm("try { eval('class A {}; class B {}'); } catch (_) {} var after = 1;");
        classes.set_dynamic_code_limits(1024, 4096, 16, 128, 1);
        classes
            .run()
            .expect_err("two ClassDefs exceed the concrete class allowance");
        assert_eq!(
            classes.instr_rec.as_ref().unwrap().exhaustion,
            Some(ResourceExhaustion::DynamicClasses)
        );
        assert!(
            classes.eval_funcs.is_empty(),
            "rejection must precede leaks"
        );
        assert!(
            classes.eval_classes.is_empty(),
            "rejection must precede leaks"
        );
    }
}
