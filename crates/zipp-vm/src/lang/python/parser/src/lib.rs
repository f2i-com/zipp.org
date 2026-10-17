//! ZIPP's Python front end: a byte-oriented lexer ([`lexer`]), a compact
//! arena AST ([`ast`]), a recursive-descent parser ([`parse`]) for Python
//! 3.13 syntax, and the tree ZIPP's emitter walks ([`tree`]), written for
//! ZIPP.
//!
//! ```
//! use zipp_pyparse::{parse, Mode, Limits};
//! let module = parse("print(f'{x=}')\n", Mode::Module, 0, Limits::NONE).unwrap();
//! assert_eq!(module.list(module.body).len(), 1);
//! ```
//!
//! Compatibility: the parser accepts what the RustPython 0.4 parser ZIPP used
//! before accepted, with the same node ranges and the same syntax errors
//! (message and offset), and also the Python 3.12/3.13 syntax it lacked:
//! PEP 701 f-strings and PEP 696 type parameter defaults. [`tree::build`]
//! lays a module out as borrowed nodes in a bump arena, in the shape of the
//! RustPython AST ZIPP's emitter was written against. See README.md.
#![forbid(unsafe_code)]

pub mod ast;
pub mod intern;
pub mod lexer;
mod numbers;
mod numbers_display;
mod parser;
mod strings;
pub mod token;
pub mod tree;
pub mod unicode_names;

pub use lexer::{Error, Limits, Mode};
pub use parser::parse;
