use alloc::string::String;
use core::fmt;

use zipp_quants::dtype::UnknownDtype;

pub type Result<T> = core::result::Result<T, GgufError>;

/// Every way a GGUF header can fail to be one. There is no `Io` variant: this
/// reader is handed bytes and never opens anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GgufError {
    BadMagic(u32),
    UnsupportedVersion(u32, &'static [u32]),
    UnknownValueType(u32),
    UnknownGgmlType(u32),
    UnknownDtype(UnknownDtype),
    BadUtf8,
    MissingKey(String),
    TypeMismatch { key: String, expected: &'static str, actual: &'static str },
    Truncated { offset: u64, needed: u64 },
    NestedArray(String),
    TooManyDims { name: String, n_dims: u32 },
    NotBlockAligned { name: String, block: usize, numel: u64 },
}

impl From<UnknownDtype> for GgufError {
    fn from(e: UnknownDtype) -> Self { Self::UnknownDtype(e) }
}
impl From<core::str::Utf8Error> for GgufError {
    fn from(_: core::str::Utf8Error) -> Self { Self::BadUtf8 }
}
impl From<alloc::string::FromUtf8Error> for GgufError {
    fn from(_: alloc::string::FromUtf8Error) -> Self { Self::BadUtf8 }
}

impl fmt::Display for GgufError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadMagic(m) => write!(f, "not a GGUF file: bad magic 0x{m:08x}"),
            Self::UnsupportedVersion(v, ok) => write!(f, "unsupported GGUF version {v} (supported: {ok:?})"),
            Self::UnknownValueType(t) => write!(f, "unknown ValueType tag {t}"),
            Self::UnknownGgmlType(t) => write!(f, "unknown ggml dtype tag {t}"),
            Self::UnknownDtype(e) => write!(f, "{e}"),
            Self::BadUtf8 => write!(f, "string was not valid UTF-8"),
            Self::MissingKey(k) => write!(f, "expected metadata key `{k}` was not found"),
            Self::TypeMismatch { key, expected, actual } =>
                write!(f, "metadata key `{key}` had wrong type: expected {expected}, got {actual}"),
            Self::Truncated { offset, needed } =>
                write!(f, "file truncated: needed {needed} more bytes at offset {offset}"),
            Self::NestedArray(k) => write!(f, "nested arrays of arrays are not supported (key `{k}`)"),
            Self::TooManyDims { name, n_dims } =>
                write!(f, "tensor `{name}` has unsupported {n_dims} dimensions (max 4)"),
            Self::NotBlockAligned { name, block, numel } =>
                write!(f, "tensor `{name}` length not a multiple of block size {block} (numel = {numel})"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for GgufError {}
