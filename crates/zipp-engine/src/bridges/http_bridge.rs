//! HTTP bridge for synchronous outbound requests.
//!
//! The VM is single-threaded, so [`HttpBridge::get`] et al. block the
//! script until the host returns. For non-blocking network I/O, use the
//! async yield protocol in [`crate::host_bridge`] and dispatch the
//! request from JavaScript or an async runtime.
//!
//! # Writing an implementation
//!
//! ```no_run
//! use zipp_engine::http_bridge::{HttpBridge, HttpResponse};
//!
//! pub struct UreqBridge;
//!
//! impl HttpBridge for UreqBridge {
//!     fn get(&self, url: &str) -> Result<HttpResponse, String> {
//!         # fn fetch(_url: &str) -> Result<(u16, String), String> { unimplemented!() }
//!         let (status, body) = fetch(url)?;
//!         Ok(HttpResponse {
//!             status,
//!             ok: (200..300).contains(&status),
//!             body,
//!         })
//!     }
//!     # fn post(&self, _u: &str, _b: &str, _ct: &str) -> Result<HttpResponse, String> { unimplemented!() }
//!     # fn put(&self, _u: &str, _b: &str, _ct: &str) -> Result<HttpResponse, String> { unimplemented!() }
//!     # fn delete(&self, _u: &str) -> Result<HttpResponse, String> { unimplemented!() }
//! }
//! ```

/// Flattened response surface. The VM only sees these three fields —
/// headers, trailers, and the raw byte body are deliberately hidden to
/// keep the sandbox narrow. If callers need richer shapes, add a
/// domain-specific bridge.
pub struct HttpResponse {
    /// HTTP status code (e.g. `200`, `404`, `503`).
    pub status: u16,
    /// UTF-8 decoded body. Binary responses are implementation-defined
    /// — most bridges base64-encode or error.
    pub body: String,
    /// Convenience: `status` is in `[200, 300)`.
    pub ok: bool,
}

/// Synchronous HTTP client surface.
pub trait HttpBridge {
    /// `GET url`.
    fn get(&self, url: &str) -> Result<HttpResponse, String>;
    /// `POST url` with `body` under the given `content_type`.
    fn post(&self, url: &str, body: &str, content_type: &str) -> Result<HttpResponse, String>;
    /// `PUT url` with `body` under the given `content_type`.
    fn put(&self, url: &str, body: &str, content_type: &str) -> Result<HttpResponse, String>;
    /// `DELETE url`. Body, when relevant, is the host's concern.
    fn delete(&self, url: &str) -> Result<HttpResponse, String>;
}
