//! Runtime value model.
//!
//! * [`value`] — The NaN-boxed [`value::Value`] type. A 64-bit POD that
//!   encodes `i32`, `f64`, `bool`, `null`, `undefined`, inline strings up to
//!   six bytes, and heap references. Also exposes the [`value::Heap`] arena.
//! * [`object`] — The [`object::Object`] enum (String, Array, Hash, Map,
//!   Function, Class, Promise, Generator, Closure, …) plus helpers used by
//!   the compiler and VM.
//! * [`intern`] — Process-global string interner backing symbol IDs used
//!   throughout the pipeline.

pub mod globals;
pub mod intern;
pub mod object;
pub mod value;
