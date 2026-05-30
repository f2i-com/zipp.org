//! Tier-2 emit layer: allocated IR → native machine code.
//!
//! Consumes [`Allocation`](super::regalloc::Allocation) + [`IrFunction`](super::ir::IrFunction)
//! and produces an executable code buffer. The caller (currently a
//! unit-test harness; in phase 4d the VM itself) invokes the emitted
//! function via the Win64 ABI:
//!
//! ```text
//! extern "win64" fn(
//!     regs:    *mut u64,    // bytecode register window
//!     consts:  *const u64,  // pre-boxed constants
//!     globals: *mut u64,    // globals array
//!     vm_ptr:  *mut u8,     // reserved for helper calls (phase 4d+)
//! ) -> u64;                 // NaN-boxed return value
//! ```
//!
//! This phase (4c) lands the baseline emission — enough to run the
//! typed I32/Bool subset end-to-end. Generic (boxed-add), F64, heap
//! access, speculation checks, and runtime-helper calls are rejected
//! with [`EmitError::Unsupported`] for now. Phases 4d/5/6 extend
//! coverage.
//!
//! ## Architecture gating
//!
//! The actual emitter lives in [`x86_64`] and is only compiled when
//! the host is x86-64 and the `djit` feature (which brings in
//! `dynasmrt`) is active. On other platforms or when the feature is
//! off, the stub [`emit`] returns an error — callers then fall back
//! to tier 1 / tier 0.

use super::ir::IrFunction;
use super::regalloc::Allocation;

#[cfg(all(feature = "djit", target_arch = "x86_64"))]
mod x86_64;

#[cfg(all(feature = "djit", target_arch = "x86_64"))]
pub use x86_64::EmittedFunction;

/// What can go wrong during tier-2 emit.
///
/// All variants are recoverable at the call site: the tier-2 pipeline
/// drops the emission attempt and the function continues running in
/// tier 1 / tier 0.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EmitError {
    /// The IR contains an op or terminator shape the emitter doesn't
    /// handle yet. Arg is a short human-readable label for logging;
    /// production callers treat all variants identically.
    Unsupported(&'static str),
    /// Building the native-code buffer failed (out of memory, W^X
    /// denied, etc.). Effectively never fires on desktop targets.
    AssemblerFailed,
    /// The host platform doesn't have a tier-2 emit backend. Fired
    /// from the non-x86_64 stub.
    NoBackend,
}

impl std::fmt::Display for EmitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EmitError::Unsupported(what) => write!(f, "tier-2 emit: unsupported {what}"),
            EmitError::AssemblerFailed => f.write_str("tier-2 emit: assembler failed"),
            EmitError::NoBackend => f.write_str("tier-2 emit: no backend for host arch"),
        }
    }
}

impl std::error::Error for EmitError {}

/// Emit native code for `func` using the allocation in `alloc`.
///
/// On non-x86_64 builds this returns [`EmitError::NoBackend`]; the
/// caller should treat that as "tier 2 isn't available, fall back."
#[cfg(all(feature = "djit", target_arch = "x86_64"))]
pub fn emit(func: &IrFunction, alloc: &Allocation) -> Result<EmittedFunction, EmitError> {
    x86_64::emit(func, alloc)
}

/// Stub used on platforms without a native backend.
#[cfg(not(all(feature = "djit", target_arch = "x86_64")))]
pub fn emit(_func: &IrFunction, _alloc: &Allocation) -> Result<(), EmitError> {
    Err(EmitError::NoBackend)
}
