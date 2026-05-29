//! ZIPP compiler core + register VM.
//!
//! Pipeline (v0): `source -> lexer -> parser -> checker -> ir (bytecode) -> vm`.
//! Each module maps onto a stage in PLAN.md; they live as modules here for v0
//! and can be split into separate crates (zipp-frontend / zipp-checker /
//! zipp-zhir / zipp-zmir / zipp-runtime) as the language grows.
//!
//! The VM emits an execution [`vm::TraceStep`] stream that the optional
//! `zipp-zk` crate proves with a zk-STARK.

pub mod ast;
pub mod check;
pub mod ir;
pub mod lexer;
pub mod parser;
pub mod vm;

pub use ir::Program;
pub use vm::{OpKind, RunResult, TraceStep};

/// Compile ZIPP source text to executable bytecode.
pub fn compile(src: &str) -> Result<Program, String> {
    let tokens = lexer::lex(src)?;
    let ast = parser::parse(&tokens)?;
    check::check(&ast)?;
    ir::lower(&ast)
}

/// Compile and run ZIPP source, returning the result of `main`, any printed
/// output, and the execution trace (for the zk profile).
pub fn run(src: &str) -> Result<RunResult, String> {
    let program = compile(src)?;
    vm::run(&program, false)
}
