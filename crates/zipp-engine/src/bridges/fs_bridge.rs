//! File-system bridge for host-gated file I/O.
//!
//! Sandboxed scripts never touch the real file system directly; every
//! read / write goes through the host's [`FsBridge`] implementation.
//! A well-behaved host enforces a virtual root (chroot-style) so a
//! `"../etc/passwd"` path can't escape the intended sandbox directory.
//!
//! # Writing an implementation
//!
//! ```
//! use zipp_engine::fs_bridge::FsBridge;
//! use std::path::{Path, PathBuf};
//!
//! pub struct ScopedFs { root: PathBuf }
//!
//! impl ScopedFs {
//!     fn resolve(&self, p: &str) -> Result<PathBuf, String> {
//!         let joined = self.root.join(p);
//!         let canonical = joined.canonicalize()
//!             .map_err(|e| format!("{}: {}", p, e))?;
//!         if !canonical.starts_with(&self.root) {
//!             return Err(format!("path escapes sandbox: {}", p));
//!         }
//!         Ok(canonical)
//!     }
//! }
//!
//! impl FsBridge for ScopedFs {
//!     fn read_file(&self, path: &str) -> Result<String, String> {
//!         let p = self.resolve(path)?;
//!         std::fs::read_to_string(&p).map_err(|e| e.to_string())
//!     }
//!     # fn write_file(&mut self, _path: &str, _content: &str) -> Result<(), String> { unimplemented!() }
//!     # fn append_file(&mut self, _path: &str, _content: &str) -> Result<(), String> { unimplemented!() }
//!     # fn exists(&self, _path: &str) -> bool { false }
//!     # fn list_dir(&self, _path: &str) -> Result<Vec<String>, String> { unimplemented!() }
//!     # fn delete_file(&mut self, _path: &str) -> Result<(), String> { unimplemented!() }
//!     # fn mkdir(&mut self, _path: &str) -> Result<(), String> { unimplemented!() }
//! }
//! ```

/// Synchronous file-system operations. Paths are plain strings —
/// interpretation is left entirely to the implementation.
pub trait FsBridge {
    /// Read a UTF-8 file into a `String`. Non-UTF-8 content should
    /// produce an `Err` rather than lossy conversion.
    fn read_file(&self, path: &str) -> Result<String, String>;
    /// Overwrite (or create) a file with the given UTF-8 content.
    fn write_file(&mut self, path: &str, content: &str) -> Result<(), String>;
    /// Append to an existing file; creating it if missing is
    /// implementation-defined.
    fn append_file(&mut self, path: &str, content: &str) -> Result<(), String>;
    /// True if a file or directory is visible at `path`.
    fn exists(&self, path: &str) -> bool;
    /// Names of entries inside a directory. Ordering is
    /// implementation-defined.
    fn list_dir(&self, path: &str) -> Result<Vec<String>, String>;
    /// Remove a single file. Errors on directories.
    fn delete_file(&mut self, path: &str) -> Result<(), String>;
    /// Create a directory, including any missing parents.
    fn mkdir(&mut self, path: &str) -> Result<(), String>;
}
