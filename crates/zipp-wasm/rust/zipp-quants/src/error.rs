use core::fmt;

use crate::dtype::GgmlType;

pub type Result<T> = core::result::Result<T, QuantError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuantError {
    Unsupported(GgmlType),
    NotBlockAligned { n: usize, block: usize },
    WrongLength { got: usize, expected: usize },
}

impl fmt::Display for QuantError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(t) => write!(f, "unsupported dtype for dequantization: {t:?}"),
            Self::NotBlockAligned { n, block } =>
                write!(f, "element count {n} not a multiple of block size {block}"),
            Self::WrongLength { got, expected } =>
                write!(f, "source byte length {got} != expected {expected}"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for QuantError {}
