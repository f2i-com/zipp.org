//! Environment variable access + host-side logging.
//!
//! Server-side embedders typically wire this to `std::env::var` and a
//! structured logger (`tracing`, `slog`, etc.). Browser/WASM builds
//! usually pass a no-op implementation and let the JS runtime handle
//! logging via `console.*`.
//!
//! # Writing an implementation
//!
//! ```
//! use zipp_engine::env_bridge::EnvBridge;
//!
//! pub struct StdEnv;
//!
//! impl EnvBridge for StdEnv {
//!     fn get(&self, name: &str) -> Option<String> {
//!         std::env::var(name).ok()
//!     }
//!     fn keys(&self) -> Vec<String> {
//!         std::env::vars().map(|(k, _)| k).collect()
//!     }
//!     fn log(&self, level: &str, message: &str) {
//!         eprintln!("[{}] {}", level, message);
//!     }
//! }
//! ```

/// Read-only environment access and host logging.
pub trait EnvBridge {
    /// Return the value of `name`, or `None` if unset. For security,
    /// hosts often allowlist which variables they expose.
    fn get(&self, name: &str) -> Option<String>;
    /// Enumerate visible variable names. Ordering is
    /// implementation-defined.
    fn keys(&self) -> Vec<String>;
    /// Stream a log line. `level` is one of `"info"`, `"warn"`, or
    /// `"error"` — the default is a no-op so WASM builds can ignore it
    /// and let the JS runtime's `console` handle the output.
    fn log(&self, _level: &str, _message: &str) {}
}
