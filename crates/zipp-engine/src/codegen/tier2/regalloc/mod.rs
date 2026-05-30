//! Register allocator for the tier-2 IR.
//!
//! Ships:
//!
//! * [`liveness`] — per-block live-in / live-out sets via a backward
//!   dataflow fixed-point.
//! * [`linear_scan`] — SSA-based linear-scan allocation over a
//!   reverse-postorder linearisation of the function.
//! * [`parallel_move`] — SSA-deconstruction helper that serialises
//!   a parallel-move set (jump args → successor block-param slots)
//!   into a linear sequence, introducing scratch slots only when a
//!   location cycle forces it.
//!
//! Phase 4c is the IR→dynasm emission — it consumes the outputs of
//! this module directly.
//!
//! Nothing here touches runtime execution — the allocator produces a
//! [`Allocation`] record that downstream emission consumes. Tests run
//! the analysis end-to-end and assert every ValueId gets a location.
//!
//! ## Choice: linear-scan, not graph colouring
//!
//! Linear-scan is O(n · log k) where k is the register count and n is
//! the number of live intervals — fast enough that we can tier-up
//! cheaply, and well-documented (Poletto & Sarkar 1999 for the basic
//! version, Wimmer & Franz 2010 for the SSA variant). Graph colouring
//! (Chaitin, Briggs) produces slightly tighter allocations on some
//! shapes but is substantially more code and harder to debug. The
//! trade-off picks linear-scan. The `regalloc2` crate uses the same
//! algorithm — we could pull it in, but writing our own keeps the
//! tier-2 module self-contained and forces us to understand every
//! corner.
//!
//! ## Physical register model
//!
//! Exposed as abstract integers 0..[`NUM_GP_REGS`]. The emit phase
//! will map them onto concrete x86-64 registers — picking a subset
//! that avoids ABI-reserved ones and leaves room for scratch. 8 is a
//! conservative first cut: fits comfortably in x86-64's 16 GP
//! registers after excluding rsp, rbp, and the four Windows-x64 call
//! arguments (rcx, rdx, r8, r9) that the existing tier-1 code uses.

use std::collections::BTreeMap;

use super::ir::{BlockId, IrFunction, ValueId};

pub mod linear_scan;
pub mod liveness;
pub mod parallel_move;

/// Number of abstract general-purpose registers available for value
/// holding. The emission phase maps these onto physical x86-64 regs.
pub const NUM_GP_REGS: u8 = 8;

/// Where a particular SSA value lives after allocation.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum Location {
    /// In a physical register. Register index is `0..NUM_GP_REGS`.
    Reg(u8),
    /// Spilled to a stack slot at the given byte offset from the
    /// tier-2 frame base. Slot width is 8 bytes (NaN-boxed Value).
    Spill(u32),
}

/// Result of allocation: where each value lives, plus the total
/// spill footprint and the chosen block execution order.
#[derive(Clone, Debug)]
pub struct Allocation {
    /// Location per ValueId. Sparse: values that never get a location
    /// (e.g. void-op results like `StoreGlobal`) are absent.
    pub locations: BTreeMap<ValueId, Location>,
    /// Number of spill slots used. Times 8 = stack footprint.
    pub num_spill_slots: u32,
    /// Blocks in emission order (reverse postorder of the CFG).
    pub block_order: Vec<BlockId>,
}

/// Allocate registers for `func`, producing an [`Allocation`].
///
/// Runs liveness analysis, then the linear-scan pass. Never fails —
/// spill slots are always available, so if a value can't fit a
/// register it gets spilled.
pub fn allocate(func: &IrFunction) -> Allocation {
    let block_order = liveness::compute_rpo(func);
    let live = liveness::compute(func, &block_order);
    linear_scan::allocate(func, &block_order, &live, NUM_GP_REGS)
}
