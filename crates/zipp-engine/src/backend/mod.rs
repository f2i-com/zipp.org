//! Bytecode compilation.
//!
//! ```text
//! AST (frontend::ast)  ──►  rcompiler  ──►  Bytecode  ──►  vm::rvm
//!                                           (shape)       (dispatch)
//! ```
//!
//! The register path ([`rcompiler`], opcodes in [`rcode`]) is the only
//! bytecode pipeline — the legacy stack compiler and its dispatch loop
//! were retired in 0.4.0; see the CHANGELOG for the migration. Built-in
//! namespaces like `Math` / `JSON` / `Array` live in
//! [`crate::runtime::globals`].
//!
//! [`bytecode::Bytecode`] is the shared container the compiler produces
//! and the VM consumes.

pub mod bytecode;
pub mod rcode;
pub mod rcompiler;
pub mod validate;
