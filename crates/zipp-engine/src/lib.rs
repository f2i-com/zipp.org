//! # Zipp Core
//!
//! A high-performance, sandboxed JavaScript-subset execution engine for Rust
//! and WebAssembly. Zipp provides a register-based bytecode VM with an
//! optional x86-64/AArch64 JIT backend.
//!
//! ## Crate layout
//!
//! The engine is organised into six top-level modules reflecting the
//! compilation pipeline:
//!
//! ```text
//!     frontend  ──►  backend  ──►  vm  ──►  codegen
//!         ▲             ▲          ▲          ▲
//!         │             │          │          │
//!      runtime ◄────────┴──────────┴──────────┘
//!                      │
//!                  bridges (host I/O)
//! ```
//!
//! * [`frontend`] — Tokenising, parsing, and import resolution. Input is source
//!   text, output is a [`frontend::ast::Program`].
//! * [`runtime`] — The NaN-boxed [`runtime::value::Value`] type, heap
//!   [`runtime::object::Object`] variants, and the global string interner.
//! * [`backend`] — Bytecode definitions, the AST → bytecode compiler
//!   ([`backend::rcompiler`]), and the load-time validator
//!   ([`backend::validate`]).
//! * [`vm`] — The register VM execution loop and supporting heap management.
//! * [`codegen`] — Native JIT backend. [`codegen::djit`] is a dynasm-based
//!   single-pass JIT, split per architecture.
//! * [`bridges`] — Host integration surfaces (db, http, fs, draw, …).
//!
//! ## Quick start
//!
//! ```no_run
//! use zipp_engine::engine::ZippEngine;
//!
//! let engine = ZippEngine::default();
//! let v = engine.eval("let x = 7; x * 6").unwrap();
//! assert_eq!(v.inspect(), "42");
//! ```
//!
//! ## Features
//!
//! | Flag   | Default | Purpose                                         |
//! |--------|---------|-------------------------------------------------|
//! | `djit` | no      | dynasm-based x86-64/AArch64 JIT. Opt-in because |
//! |        |         | the helpers use the Windows x64 ABI and a plain |
//! |        |         | `cargo build` on Linux/macOS x86-64 would       |
//! |        |         | otherwise hit `compile_error!`. The CLI/bench   |
//! |        |         | crates enable it automatically on supported     |
//! |        |         | targets (Windows x86-64 + AArch64).             |
//! | `zkvm` | no      | Reserved for zero-knowledge execution traces.   |
//!
//! ## Security model & trust boundary
//!
//! Zipp is a sandbox: untrusted source text goes in, bounded
//! execution comes out. The trust boundary is **the bytecode format**:
//!
//! * **Scripts are untrusted.** They go through the full lexer →
//!   parser → register compiler pipeline. The compiler is the only
//!   code path that produces a [`backend::bytecode::Bytecode`].
//! * **Bytecode is trusted.** The register VM's hot-path dispatch
//!   relies on ~150 `unsafe` operations that assume each register /
//!   constant / heap index / opcode byte is in range. Those
//!   invariants are *enforced at compile time* by the compiler and
//!   *verified at load time* by [`backend::validate::validate`]; the
//!   runtime dispatch does not re-check them.
//!
//! What this means in practice:
//!
//! * A bug in the compiler that emits an invalid register or jump
//!   target is caught at load time as
//!   [`error::ZippError::Compile`], not as runtime UB. (See
//!   [`backend::validate`].)
//! * A heap index with an out-of-range bits pattern will trip a
//!   release-mode `assert!` in [`runtime::value::Heap::get`] rather
//!   than read past the end of the heap.
//! * Resource limits ([`config::ZippConfig::max_instructions`],
//!   `max_wall_time_ms`, `max_heap_objects`, `max_heap_bytes`, and
//!   the per-thread interned-symbol cap in [`runtime::intern`]) are
//!   re-checked inside callback-taking builtins (`Array.sort`,
//!   `map`, `filter`, `reduce`, …) so a hostile callback can't
//!   sidestep them.
//! * Regex compilation is capped at 1 MiB of program / DFA memory
//!   (see [`crate::vm::VM::build_regex` implementation detail]).
//! * The async host-call queue caps `PendingHostCall.args` payload
//!   at 16 MiB so a script can't back-pressure the host.
//!
//! A full audit lives in `SECURITY-AUDIT.md` at the repo root; that
//! document is the canonical description of the threat model and is
//! updated whenever this surface changes.

#![allow(clippy::too_many_arguments)]

// ── Pipeline modules ────────────────────────────────────────────────────────
pub mod frontend;
pub mod runtime;
pub mod backend;
pub mod vm;
pub mod codegen;
pub mod bridges;

// ── Top-level facades ───────────────────────────────────────────────────────
pub mod config;
pub mod engine;
pub mod error;

pub use error::ZippError;

// ── Flat re-exports ─────────────────────────────────────────────────────────
//
// Internal source files refer to sibling modules via `crate::<name>` paths
// (e.g. `use crate::ast::Program;`). Keeping these aliases at the crate root
// means the algorithmic code — ~49 KLoC of carefully tuned lexer / parser /
// compiler / VM / JIT — did not have to be rewritten just to sit inside the
// new folder layout. They are also part of the stable public surface.

pub use frontend::{ast, imports, lexer, parser, token};
pub use runtime::{globals, intern, object, value};
pub use backend::{bytecode, rcode, rcompiler};
pub use codegen::djit;
pub use vm::rvm;
pub use bridges::{
    db_bridge, draw_bridge, env_bridge, fs_bridge, host_bridge, http_bridge, input_bridge,
    layout_bridge, local_storage,
};
