//! Public error types for the [`crate::engine::ZippEngine`] surface.
//!
//! Previously every fallible engine method returned `Result<_, String>`,
//! which forced embedders into brittle substring checks ("if err contains
//! 'Uncaught throw' …"). [`ZippError`] is a discriminated sum type
//! that makes the common failure modes pattern-matchable while still
//! rendering a familiar string via [`std::fmt::Display`].
//!
//! ## Variants at a glance
//!
//! | Variant                 | When you see it                                          |
//! |-------------------------|----------------------------------------------------------|
//! | [`Parse`]               | Source text could not be tokenised or parsed.            |
//! | [`Compile`]             | AST → bytecode failed (e.g. `const x = 1; x = 2`).       |
//! | [`UncaughtThrow`]       | A `throw` statement reached the top level.               |
//! | [`ExecutionLimit`]      | Instruction count / wall-time / heap limit exceeded.     |
//! | [`TypeError`]           | A runtime type-check failed.                             |
//! | [`StackOverflow`]       | Call/value stack overran its bounds.                     |
//! | [`InvalidOpcode`]       | Bytecode corruption (should never happen — bug indicator). |
//! | [`Runtime`]             | Everything else from the VM.                             |
//!
//! [`Parse`]:           ZippError::Parse
//! [`Compile`]:         ZippError::Compile
//! [`UncaughtThrow`]:   ZippError::UncaughtThrow
//! [`ExecutionLimit`]:  ZippError::ExecutionLimit
//! [`TypeError`]:       ZippError::TypeError
//! [`StackOverflow`]:   ZippError::StackOverflow
//! [`InvalidOpcode`]:   ZippError::InvalidOpcode
//! [`Runtime`]:         ZippError::Runtime
//!
//! ## Display format
//!
//! [`ZippError`] renders to the same strings embedders previously
//! matched on, so existing integrations keep working:
//!
//! ```text
//! Parser errors: …
//! Compile error: …
//! Uncaught throw: <value>
//! Exceeded 5000ms wall time
//! TypeError: …
//! Stack overflow
//! Invalid opcode <byte>
//! VM error: <details>
//! ```

use std::fmt;

use crate::object::Object;
use crate::value::{val_to_obj, Heap, Value};

/// Public error type for the engine surface. See the [module
/// documentation](self) for the variant catalogue and display format.
#[derive(Debug, Clone)]
pub enum ZippError {
    /// The source text failed to tokenise or parse. Contains the
    /// parser's messages joined with `, `.
    Parse(String),
    /// The AST was valid but bytecode emission failed — for example,
    /// assigning to a `const` binding or using `return` outside a
    /// function.
    Compile(String),
    /// A `throw` statement propagated to the top level. The thrown
    /// value is rendered into the message lazily; if you need the
    /// original heap value you'll want to run the VM yourself rather
    /// than going through [`crate::engine::ZippEngine::eval`].
    UncaughtThrow(String),
    /// The VM exhausted one of its quotas (`max_instructions`,
    /// `max_wall_time_ms`, `max_heap_objects`, `max_heap_bytes`) or
    /// was asked to stop via the external abort flag.
    ExecutionLimit(String),
    /// A runtime type-check (coercion, builtin receiver, etc.)
    /// failed.
    TypeError(String),
    /// The call stack or value stack overflowed. A deeper recursive
    /// function or a larger `STACK_SIZE` is needed.
    StackOverflow,
    /// An opcode byte outside the known range was encountered. This
    /// means the bytecode is corrupt and is always an engine bug.
    InvalidOpcode(u8),
    /// Catch-all for VM-level failures that don't have a dedicated
    /// variant. Carries the `Debug` rendering of the underlying
    /// `VMError`.
    Runtime(String),
}

impl ZippError {
    /// Rebuild the legacy `VM error: ...` rendering that callers
    /// sometimes substring-match on. Prefer matching on the variant
    /// directly; this is here for transitional code paths.
    pub fn legacy_string(&self) -> String {
        self.to_string()
    }

    /// Convenience: substring check against [`Display`]. Mirrors
    /// `String::contains` so existing substring-based assertions in
    /// parity tests keep working after the switch to the typed enum.
    ///
    /// Prefer matching on the variant directly in new code —
    /// `matches!(err, ZippError::UncaughtThrow(_))` is clearer and
    /// faster than `err.contains("Uncaught throw")`.
    pub fn contains(&self, needle: &str) -> bool {
        self.to_string().contains(needle)
    }

    /// Format a raw [`crate::vm::VMError`] into a typed error, dereferencing
    /// `Throw` values against the supplied heap so embedders receive the
    /// thrown value's rendered form rather than the internal `Value` bits.
    pub(crate) fn from_vm_error(err: &crate::vm::VMError, heap: &Heap) -> Self {
        use crate::vm::VMError::*;
        match err {
            Throw(val) => ZippError::UncaughtThrow(thrown_value_display(*val, heap)),
            ExecutionTimeout(msg) => ZippError::ExecutionLimit(msg.clone()),
            TypeError(msg) => ZippError::TypeError(msg.clone()),
            StackOverflow => ZippError::StackOverflow,
            StackUnderflow => ZippError::Runtime("stack underflow".into()),
            InvalidOpcode(b) => ZippError::InvalidOpcode(*b),
            InstructionOutOfBounds(ip) => {
                ZippError::Runtime(format!("instruction pointer out of bounds: {}", ip))
            }
            Yield(_) => ZippError::Runtime("generator yield outside runner".into()),
        }
    }
}

impl fmt::Display for ZippError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ZippError::Parse(msg) => write!(f, "Parser errors: {}", msg),
            ZippError::Compile(msg) => write!(f, "Compile error: {}", msg),
            ZippError::UncaughtThrow(msg) => write!(f, "Uncaught throw: {}", msg),
            ZippError::ExecutionLimit(msg) => f.write_str(msg),
            ZippError::TypeError(msg) => write!(f, "TypeError: {}", msg),
            ZippError::StackOverflow => f.write_str("Stack overflow"),
            ZippError::InvalidOpcode(b) => write!(f, "Invalid opcode {}", b),
            ZippError::Runtime(msg) => write!(f, "VM error: {}", msg),
        }
    }
}

impl std::error::Error for ZippError {}

/// Transitional `From<ZippError> for String` so `?` still works on
/// older call sites that have `-> Result<_, String>` in their signature.
impl From<ZippError> for String {
    fn from(err: ZippError) -> String {
        err.to_string()
    }
}

/// Render a thrown value the way users expect to see it in error strings.
/// `Error` / `Error`-like `Instance`s become `Name: message`; primitives
/// use their natural representation; everything else falls back to
/// [`Object::inspect`].
fn thrown_value_display(val: Value, heap: &Heap) -> String {
    let obj = val_to_obj(val, heap);
    match &obj {
        Object::String(s) => format!("\"{}\"", s),
        Object::Instance(inst) => {
            let name = inst.fields.get("name").and_then(|v| {
                if let Object::String(s) = val_to_obj(*v, heap) {
                    Some(s.to_string())
                } else {
                    None
                }
            });
            let message = inst.fields.get("message").and_then(|v| {
                if let Object::String(s) = val_to_obj(*v, heap) {
                    Some(s.to_string())
                } else {
                    None
                }
            });
            match (name, message) {
                (Some(n), Some(m)) if !m.is_empty() => format!("{}: {}", n, m),
                (Some(n), _) => n,
                _ => obj.inspect(),
            }
        }
        Object::Error(e) if !e.message.is_empty() => {
            format!("{}: {}", e.name, e.message)
        }
        Object::Error(e) => e.name.to_string(),
        Object::Undefined => "undefined".to_string(),
        Object::Null => "null".to_string(),
        _ => obj.inspect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_matches_legacy_strings() {
        assert_eq!(
            ZippError::Compile("x".into()).to_string(),
            "Compile error: x"
        );
        assert_eq!(
            ZippError::UncaughtThrow("7".into()).to_string(),
            "Uncaught throw: 7"
        );
        assert_eq!(ZippError::StackOverflow.to_string(), "Stack overflow");
        assert_eq!(
            ZippError::InvalidOpcode(255).to_string(),
            "Invalid opcode 255"
        );
    }

    #[test]
    fn into_string_preserves_display() {
        let s: String = ZippError::TypeError("bad".into()).into();
        assert_eq!(s, "TypeError: bad");
    }
}
