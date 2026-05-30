//! # Tier-2 IR
//!
//! Block-based SSA IR, one step above the register-VM bytecode. See
//! [`TIER2-JIT-PLAN.md`] §4 for the design rationale — short version:
//! block-SSA sits in the sweet spot between TAC and sea-of-nodes,
//! matches Cranelift / V8 Maglev / JSC DFG conventions, and keeps the
//! pipeline inspectable at every stage.
//!
//! ## Shape
//!
//! ```text
//!   IrFunction
//!     └── Vec<Block>
//!             ├── params:  Vec<(ValueId, ValueType)>   (block arguments)
//!             ├── ops:     Vec<(ValueId, IrOp)>        (straight-line work)
//!             └── term:    Terminator                  (exits the block)
//! ```
//!
//! There are no phi nodes — Cranelift demonstrated that *block
//! parameters* are simpler and strictly more expressive. A phi in
//! textbook SSA becomes a block parameter in our model, and each
//! incoming edge supplies its own argument list at the `Jump` /
//! `Branch` terminator. This removes an asymmetry between ordinary
//! ops and phis (they'd otherwise need different scheduling rules)
//! and makes verification + printing trivial.
//!
//! ## Layout invariants
//!
//! Enforced by [`verify`](super::ir::verify), re-checked after every
//! IR-rewriting pass:
//!
//! 1. **Single-definition.** Each `ValueId` has exactly one defining
//!    site — either a block parameter or the result of one `IrOp`.
//! 2. **Dominance.** A use of `ValueId(v)` at position P is only legal
//!    if P is dominated by the definition of `v`.
//! 3. **Terminator discipline.** Every block ends with exactly one
//!    [`Terminator`]; no fall-through between blocks.
//! 4. **Type consistency.** Operators only accept operands of the
//!    types declared in their documentation. `Check*` nodes are the
//!    only way to narrow a generic [`ValueType::Value`] to a more
//!    specific type.
//!
//! ## Phase status
//!
//! | Phase | Status | What it does |
//! |-------|--------|--------------|
//! | 1     | ✅ *here* | Types, translator, verifier, printer. No codegen. |
//! | 2     | ⏳ next | Interpreter-on-IR for differential fuzzing. |
//! | 3+    | 📋 plan | Passes, regalloc, emit, speculate, deopt. |

pub mod build;
pub mod eval;
pub mod iv;
pub mod loops;
pub mod passes;
pub mod print;
pub mod types;
pub mod verify;

pub use types::{
    Block, BlockId, BuildError, DeoptId, DeoptPoint, IrFunction, IrOp, Location, RuntimeHelper,
    Terminator, ValueId, ValueType,
};
