//! Parse-phase modules: source text → [`ast::Program`].
//!
//! * [`token`] — Keyword / operator tables and the `Token` type.
//! * [`lexer`] — Tokenises source text, handling strings, regex, template
//!   literals, numeric literals and ASI.
//! * [`ast`] — AST node definitions used by every downstream phase.
//! * [`parser`] — Pratt parser that consumes [`token::Token`]s and emits an
//!   [`ast::Program`].
//! * [`imports`] — Resolves `import "..."` statements by inlining file contents
//!   (with circular-import protection).

pub mod ast;
pub mod imports;
pub mod lexer;
pub mod parser;
pub mod token;
