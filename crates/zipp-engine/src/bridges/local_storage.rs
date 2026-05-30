//! Key/value persistence bridge — the host side of `localStorage`.
//!
//! Implementations typically wrap a SQLite table (native), an IndexedDB
//! object store (browser), or a plain in-memory `HashMap` (tests).
//!
//! # Writing an implementation
//!
//! ```
//! use std::collections::HashMap;
//! use zipp_engine::local_storage::LocalStorageBridge;
//!
//! #[derive(Default)]
//! pub struct MemoryStorage(HashMap<String, String>);
//!
//! impl LocalStorageBridge for MemoryStorage {
//!     fn get_item(&self, key: &str) -> Option<String> {
//!         self.0.get(key).cloned()
//!     }
//!     fn set_item(&mut self, key: &str, value: &str) {
//!         self.0.insert(key.into(), value.into());
//!     }
//!     fn remove_item(&mut self, key: &str) {
//!         self.0.remove(key);
//!     }
//!     fn clear(&mut self) {
//!         self.0.clear();
//!     }
//! }
//! ```
//!
//! The trait deliberately mirrors the browser's [`Storage`] API so
//! existing JavaScript shims translate one-to-one.
//!
//! [`Storage`]: https://developer.mozilla.org/en-US/docs/Web/API/Storage

/// Host-pluggable key/value store backing `localStorage.*`.
pub trait LocalStorageBridge {
    /// Read a key. Returns `None` for missing keys.
    fn get_item(&self, key: &str) -> Option<String>;
    /// Write or overwrite a key.
    fn set_item(&mut self, key: &str, value: &str);
    /// Remove a key (no-op if it doesn't exist).
    fn remove_item(&mut self, key: &str);
    /// Drop every key in the store.
    fn clear(&mut self);
}
