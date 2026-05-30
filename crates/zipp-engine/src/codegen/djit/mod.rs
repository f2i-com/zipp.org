//! dynasm-based JIT compiler.
//!
//! Compiles hot register-VM functions to native machine code through
//! the [`dynasmrt`] single-pass assembler. Each architecture has its
//! own back-end:
//!
//! * [`x86_64`] — primary target, full opcode coverage, the one
//!   exercised by the native benchmark.
//! * [`aarch64`] — secondary target, covers the same opcode set so
//!   Apple Silicon / Linux aarch64 builds get the JIT too.
//!
//! Both files define the same public surface (`DynasmJit`,
//! `DjitFunction`, `DJIT_THRESHOLD`) and the `cfg`-gated re-exports
//! below pick whichever one matches the host target. A `cfg(not(feature
//! = "djit"))` stub is provided at the bottom so callers compile even
//! when the JIT feature is disabled — e.g. on `wasm32`, where dynasm
//! does not run.
//!
//! Prior to 0.6 this whole file was a single 4 040-line `djit.rs` with
//! two inline `mod impl_djit { … }` blocks. Splitting by architecture
//! keeps each file under 3 k lines and makes it easier to grep for
//! arch-specific instruction selection.

// Route the cfg-gated module declarations at per-arch files via
// `#[path]` so there's only one `impl_djit` name and the public
// re-export below doesn't need its own cfg fork.

#[cfg(all(feature = "djit", target_arch = "x86_64"))]
#[path = "x86_64.rs"]
mod impl_djit;

#[cfg(all(feature = "djit", target_arch = "aarch64"))]
#[path = "aarch64.rs"]
mod impl_djit;

// ── Public API ──────────────────────────────────────────────────────────────

#[cfg(feature = "djit")]
pub use impl_djit::{DjitFunction, DynasmJit, DJIT_THRESHOLD};

/// Stub `DynasmJit` used when the `djit` feature is disabled (e.g. on
/// `wasm32`). Every method is a no-op.
#[cfg(not(feature = "djit"))]
#[derive(Default)]
pub struct DynasmJit;

#[cfg(not(feature = "djit"))]
impl DynasmJit {
    pub fn new() -> Self {
        DynasmJit
    }
    pub fn clear_deferred(&mut self) {}
    pub fn set_deferred(&mut self, _func_key: usize) {}
}
