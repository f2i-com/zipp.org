//! # Tier-2 Optimising JIT
//!
//! Implements the tier-2 pipeline described in `TIER2-JIT-PLAN.md` at
//! the repo root. It sits between the existing tier-0 register VM and
//! tier-1 dynasm baseline JIT:
//!
//! ```text
//!    bytecode
//!       │  ──► tier-0 (rvm/)                 always available
//!       │  ──► tier-1 (codegen/djit/)        compiled after 4 calls
//!       └──► tier-2 (codegen/tier2/)         compiled when hot enough
//!               ├── ir         bytecode → SSA IR + verifier, printer,
//!               │              passes (const-fold, DCE, copy-prop,
//!               │              CFG simplify, LICM, IV reduce)
//!               ├── regalloc   liveness, linear-scan allocator,
//!               │              parallel-move resolver
//!               └── emit       allocated IR → native x86-64 (phase 4c)
//! ```
//!
//! Current phase (4c): baseline native emission for the typed
//! I32/Bool subset. Generic arithmetic, F64, heap access, speculation
//! checks, and runtime-helper calls are rejected by the emitter until
//! later phases fill them in. There is no runtime integration yet —
//! tier 2 produces executable buffers that unit tests invoke
//! directly; wiring into `VM::Call` is phase 4d.
//!
//! The native emitter is gated on `#[cfg(all(feature = "djit",
//! target_arch = "x86_64"))]` so it has **zero effect** on the
//! shipping binary until the rest of the pipeline is wired up — and
//! is automatically excluded from compilation on targets where the
//! tier-2 code generator will never run. The IR / regalloc / passes
//! are arch-independent and compile in any configuration so unit
//! tests can exercise them without an emit back-end.
//!
//! ## Why a separate module and not a rewrite?
//!
//! The existing tier-1 JIT is doing good work (1.5–1.8× V8 on several
//! benchmarks); tier 2 is additive for the remaining cases where the
//! baseline's single-pass emission leaves optimisation on the table.
//! Keeping it in its own folder means:
//!
//! * Tier-1 code doesn't change shape just because tier-2 wants a
//!   particular IR convention.
//! * Feature-flagging `tier2` cleanly toggles the new code path.
//! * The (~14-week) phased plan can land incrementally without ever
//!   breaking the existing build.

pub mod cache;
pub mod emit;
pub mod ir;
pub mod regalloc;
pub mod runtime;

#[cfg(all(feature = "djit", target_arch = "x86_64"))]
pub use cache::{Tier2Jit, TIER2_THRESHOLD};

// ── Top-level compile entry ──────────────────────────────────────────

#[cfg(all(feature = "djit", target_arch = "x86_64"))]
pub use pipeline::{try_compile, CompileError};

#[cfg(all(feature = "djit", target_arch = "x86_64"))]
mod pipeline {
    //! Public pipeline: bytecode → IR → passes → verify → regalloc →
    //! native. Tier-2-facing callers run through [`try_compile`] and
    //! get back an executable buffer or a coarse-grained failure
    //! reason. Finer-grained failure info (which op, which pass) is
    //! available by running the stages individually — `try_compile`
    //! intentionally collapses that to keep callers simple.

    use std::rc::Rc;

    use crate::object::Object;

    use super::emit::{self, EmittedFunction, EmitError};
    use super::ir::passes::inline as inline_pass;
    use super::ir::passes::speculate::{self, SpeculateConfig};
    use super::ir::{self, BuildError, IrFunction};
    use super::regalloc;

    /// What can go wrong while compiling a function to tier-2.
    ///
    /// Callers should treat all variants as recoverable: drop the
    /// compile attempt, optionally blacklist the function so we
    /// don't retry on the next hot-count tick, and fall through to
    /// tier 1 / tier 0.
    #[derive(Clone, Debug)]
    pub enum CompileError {
        /// Bytecode → IR translation rejected the input (unsupported
        /// opcode, malformed jump target, etc.).
        Build(BuildError),
        /// IR verifier rejected either the initial translation or the
        /// post-pass IR.
        Verify(String),
        /// Native emit rejected the IR (unsupported op, assembler
        /// failure). Carries the original [`EmitError`].
        Emit(EmitError),
    }

    impl std::fmt::Display for CompileError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                CompileError::Build(e) => write!(f, "tier-2 compile: build: {e}"),
                CompileError::Verify(s) => write!(f, "tier-2 compile: verify: {s}"),
                CompileError::Emit(e) => write!(f, "tier-2 compile: emit: {e}"),
            }
        }
    }

    impl std::error::Error for CompileError {}

    /// Compile `instructions` to a tier-2 [`EmittedFunction`].
    ///
    /// Pipeline:
    ///
    /// 1. `ir::build::translate` — bytecode → IR.
    /// 2. `ir::verify` — sanity check the fresh IR.
    /// 3. `ir::passes::run_default_pipeline` — const-fold, DCE,
    ///    copy-prop, CFG simplify, LICM, IV reduce, iterated to
    ///    fixed point.
    /// 4. `ir::verify` — re-check post-pass.
    /// 5. `speculate::run` — rewrite surviving generic arithmetic
    ///    ops to their typed + checked variants. Any type mismatch
    ///    at runtime trips the phase-6 soft-deopt runtime, which
    ///    blacklists the tier-2 entry and retries via tier-1.
    /// 6. `ir::verify` — re-check post-speculation.
    /// 7. `regalloc::allocate` — liveness + linear scan.
    /// 8. `emit::emit` — native x86-64.
    ///
    /// Returns the buffered native function on success, a
    /// [`CompileError`] otherwise. No VM-side caching is performed
    /// here — the [`super::cache::Tier2Jit`] layer owns that.
    pub fn try_compile(
        instructions: &[u8],
        constants: Rc<Vec<Object>>,
        num_bytecode_regs: u16,
        num_parameters: u16,
    ) -> Result<EmittedFunction, CompileError> {
        let mut func: IrFunction =
            ir::build::translate(instructions, constants, num_bytecode_regs, num_parameters)
                .map_err(CompileError::Build)?;
        ir::verify::verify(&func).map_err(CompileError::Verify)?;
        let _iters = ir::passes::run_default_pipeline(&mut func);
        ir::verify::verify(&func).map_err(CompileError::Verify)?;
        // Speculation runs after the canonical passes so it only
        // sees generic ops that survived const-fold / DCE. Its
        // rewrites (CheckI32, CheckedAdd, BoxI32, …) are safe
        // because phase 6's deopt runtime catches any mismatch.
        let _rewrites = speculate::run(&mut func, &SpeculateConfig { speculate_i32: true });
        ir::verify::verify(&func).map_err(CompileError::Verify)?;
        // Inlining: splice monomorphic-callee bodies in place so the
        // tier-2 emitter skips the `djit_call_helper` round-trip for
        // those sites. Runs once, then the default pipeline runs
        // again to mop up const-fold and DCE opportunities that
        // inlining exposes (e.g. a callee's ConstI32 becoming a
        // constant operand in the caller's arithmetic).
        let inlined = inline_pass::run(&mut func);
        if inlined > 0 {
            ir::verify::verify(&func).map_err(CompileError::Verify)?;
            let _iters = ir::passes::run_default_pipeline(&mut func);
            ir::verify::verify(&func).map_err(CompileError::Verify)?;
            let _rewrites =
                speculate::run(&mut func, &SpeculateConfig { speculate_i32: true });
            ir::verify::verify(&func).map_err(CompileError::Verify)?;
        }
        let alloc = regalloc::allocate(&func);
        emit::emit(&func, &alloc).map_err(CompileError::Emit)
    }
}
