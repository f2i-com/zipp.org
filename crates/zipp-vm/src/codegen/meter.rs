//! Step metering for native code — the safepoint that lets an instrumented VM
//! keep its JIT.
//!
//! The interpreter charges one step per instruction in its dispatch loop. Native
//! code never iterates that loop, so a JIT'd hot loop used to be invisible to
//! both the step budget and the abort flag: it would run to completion, ignore
//! its limit entirely, and still return the right answer. That is why attaching
//! instrumentation used to switch the JIT off outright.
//!
//! # How the charge stays honest
//!
//! Native code decrements a counter **once per basic block, by that block's
//! exact bytecode-instruction count**. A basic block is straight-line: entering
//! it means executing all of it, so the charge equals what the interpreter would
//! have counted — the same number, in the same unit, in both tiers.
//!
//! The one inexactness is a block the region leaves early, through a type-guard
//! bail in its middle. That block was charged in full at its head, and the
//! interpreter then re-charges the instructions it re-executes from the bail ip.
//! So a guard bail OVER-charges, by at most one block. It is bounded — a region
//! is evicted after `OSR_DEOPT_LIMIT` bails — and it errs in the direction that
//! cannot let a script exceed its budget. (Charging at the block's tail instead
//! would remove even that, at the cost of finding every fall-through boundary in
//! six different emitters; the accuracy is not worth the surface.)
//!
//! # Why a counter on the VM rather than a stack slot
//!
//! Every region and whole-function compiler already keeps the VM pointer in
//! `rdi` for the whole body. A plain `i64` field on `Vm` is therefore reachable
//! as `[rdi + off]` with no spare register, no frame-layout change in any
//! emitter, and no address-stability problem — the offset is relative to a
//! pointer that is passed in fresh on every native entry, so it survives the
//! `Vm` being moved.
//!
//! The hot cost is two instructions per basic block, and only in a metered VM:
//!
//! ```text
//! sub QWORD [rdi + off], <block length>
//! jle => <out-of-line stub: mov [rsi], ip ; jmp flush_exit>
//! ```
//!
//! # Chunking, and why native code returns periodically
//!
//! `Vm::jit_steps` is not the whole budget — it is a slice of it, lent by
//! [`crate::vm::Vm::meter_lend`] before each native entry and reconciled after.
//! Two things fall out of that. Unspent steps are returned, so the accounting is
//! exact. And native code cannot run longer than one chunk without handing
//! control back, which is what gives the abort flag a bounded response time even
//! inside an unbounded loop — the flag is polled in Rust on every lend.

use dynasmrt::{dynasm, DynasmApi, DynasmLabelApi};

use crate::bytecode::Instr;

/// Steps lent to native code per entry. At roughly a nanosecond a step in
/// compiled code this bounds a native run to about a millisecond, which is the
/// abort flag's worst-case response time; the cost is one interpreter round trip
/// and one region re-entry per chunk, which is nothing spread over 2^20 steps.
pub const NATIVE_CHUNK: i64 = 1 << 20;

/// Where the step counter lives, as a displacement from the VM pointer the
/// compiled body holds in `rdi`.
///
/// `None` in an unmetered VM, which is the only state a default build can be in
/// — no check is emitted at all and the compiled code is byte-identical to what
/// it was before metering existed.
#[derive(Clone, Copy)]
pub(crate) struct Meter {
    pub steps_off: i32,
}

/// Emit the per-block charge: subtract `len` steps and leave for `stub` when the
/// lent chunk runs out.
///
/// Clobbers flags, so a caller relying on compare→branch fusion must drop the
/// fused compare for this ip. Every site that calls this is a basic-block head,
/// and fusion never spans one, so in practice there is nothing to drop.
#[inline]
pub(crate) fn emit_charge(
    ops: &mut dynasmrt::x64::Assembler,
    m: &Meter,
    len: u32,
    stub: dynasmrt::DynamicLabel,
) {
    dynasm!(ops
        ; sub QWORD [rdi + m.steps_off], len as i32
        ; jle => stub
    );
}

/// The per-region block map, and nothing when the VM is not metered.
pub(crate) type BlockMap = Option<(Meter, rustc_hash::FxHashMap<usize, u32>)>;

/// Build the block map for `[start, end]`, or `None` for an unmetered VM.
pub(crate) fn block_map(
    meter: Option<Meter>,
    code: &[Instr],
    start: usize,
    end: usize,
) -> BlockMap {
    let m = meter?;
    Some((m, blocks(code, start, end).into_iter().collect()))
}

/// Charge the basic block starting at `ip`, if one starts there.
///
/// Reuses the region's existing `exit_stubs` map: a stub is `mov [rsi], ip ;
/// jmp <epilogue>`, which is exactly the resume-here-in-the-interpreter exit a
/// metering bail needs. Keys cannot collide with the out-of-region branch
/// targets already in that map — those are by definition outside `[start, end]`,
/// and every block head is inside it.
///
/// Returns whether a check was emitted; a `true` means CPU flags were clobbered.
pub(crate) fn charge_block(
    ops: &mut dynasmrt::x64::Assembler,
    bm: &BlockMap,
    ip: usize,
    exit_stubs: &mut rustc_hash::FxHashMap<u32, dynasmrt::DynamicLabel>,
) -> bool {
    let Some((m, blocks)) = bm else { return false };
    let Some(&len) = blocks.get(&ip) else { return false };
    let stub = *exit_stubs.entry(ip as u32).or_insert_with(|| ops.new_dynamic_label());
    emit_charge(ops, m, len, stub);
    true
}

/// The basic blocks of `code[start..=end]`, as `(block head ip, instruction
/// count)`.
///
/// A block head is the region entry, any in-region branch target, or the
/// instruction after a branch (its fall-through). Charging every head therefore
/// charges every instruction the region can execute exactly once per pass.
pub(crate) fn blocks(code: &[Instr], start: usize, end: usize) -> Vec<(usize, u32)> {
    let mut heads = vec![false; end - start + 2];
    let mut mark = |ip: usize| {
        if ip >= start && ip <= end {
            heads[ip - start] = true;
        }
    };
    mark(start);
    for ip in start..=end {
        let target = match code[ip] {
            Instr::Jump { target }
            | Instr::JumpIfFalse { target, .. }
            | Instr::JumpIfTrue { target, .. }
            | Instr::JumpIfNotLt { target, .. }
            | Instr::JumpIfNotLe { target, .. } => target as usize,
            _ => continue,
        };
        mark(target);
        // The fall-through of a conditional branch starts a block too. For an
        // unconditional `Jump` the next ip is only reachable as a branch target,
        // which the loop above already marks — marking it here as well is
        // harmless (it is the same set).
        mark(ip + 1);
    }

    let mut out = Vec::new();
    let mut head = None;
    for ip in start..=end {
        if heads[ip - start] {
            if let Some(h) = head {
                out.push((h, (ip - h) as u32));
            }
            head = Some(ip);
        }
    }
    if let Some(h) = head {
        out.push((h, (end + 1 - h) as u32));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jmp(t: u32) -> Instr {
        Instr::Jump { target: t }
    }
    fn brf(t: u32) -> Instr {
        Instr::JumpIfFalse { cond: 0, target: t }
    }
    fn nop() -> Instr {
        Instr::LoadInt { dst: 0, val: 0 }
    }

    /// The whole point of the scheme: every instruction in the region is
    /// accounted for exactly once per pass, so the native charge is the same
    /// number the interpreter would have counted.
    fn assert_covers(code: &[Instr], start: usize, end: usize) {
        let bs = blocks(code, start, end);
        let total: u32 = bs.iter().map(|&(_, n)| n).sum();
        assert_eq!(total as usize, end - start + 1, "blocks must partition the region");
        let mut next = start;
        for (head, len) in bs {
            assert_eq!(head, next, "blocks must be contiguous and in order");
            assert!(len > 0, "a block cannot be empty");
            next = head + len as usize;
        }
    }

    #[test]
    fn a_straight_line_region_is_one_block() {
        let code = vec![nop(), nop(), nop(), nop()];
        assert_eq!(blocks(&code, 0, 3), vec![(0, 4)]);
        assert_covers(&code, 0, 3);
    }

    #[test]
    fn a_counted_loop_splits_at_the_header_and_the_latch() {
        // 0: nop            (pre-header, outside)
        // 1: header  — branch out
        // 2: body
        // 3: back-edge to 1
        let code = vec![nop(), brf(4), nop(), jmp(1), nop()];
        // Region [1,3]: head 1 (branch), fall-through 2, and 1 is a branch target.
        assert_eq!(blocks(&code, 1, 3), vec![(1, 1), (2, 2)]);
        assert_covers(&code, 1, 3);
    }

    #[test]
    fn a_branchy_body_splits_at_every_arm() {
        // 0: header, branch to 4
        // 1: then
        // 2: jump to 5
        // 3: (else, reachable only as a target)
        // 4: else target
        // 5: back-edge
        let code = vec![brf(4), nop(), jmp(5), nop(), nop(), jmp(0)];
        let bs = blocks(&code, 0, 5);
        assert_eq!(bs, vec![(0, 1), (1, 2), (3, 1), (4, 1), (5, 1)]);
        assert_covers(&code, 0, 5);
        // Nothing is charged twice and nothing is missed.
        assert_eq!(bs.iter().map(|&(_, n)| n).sum::<u32>(), 6);
    }

    #[test]
    fn out_of_region_targets_do_not_create_blocks() {
        let code = vec![nop(), brf(99), nop(), nop()];
        assert_eq!(blocks(&code, 0, 3), vec![(0, 2), (2, 2)]);
        assert_covers(&code, 0, 3);
    }

    #[test]
    fn a_single_instruction_region_is_one_block_of_one() {
        assert_eq!(blocks(&[nop()], 0, 0), vec![(0, 1)]);
    }
}
