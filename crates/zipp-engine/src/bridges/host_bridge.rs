//! Async-style host-call protocol.
//!
//! The VM is single-threaded and synchronous, so *genuinely* async
//! operations (network, timers, file I/O on an event loop) can't be
//! performed inline. Instead, a script `yield`s a
//! [`PendingHostCall`]; the engine parks the generator and exposes the
//! pending call to the embedder. The host performs the work on its
//! own schedule and resumes the VM by calling
//! [`crate::engine::ScriptState::resolve_host_callback`] (or the
//! WASM-level `resolveHostCallback`) with the resolved value.
//!
//! ```js
//! // Script side
//! function* run() {
//!   const body = yield { _kind: "Http.get", _args: ["https://api.example"] };
//!   return JSON.parse(body);
//! }
//! ```
//!
//! ```rust,ignore
//! // Host side (native)
//! let mut state = engine.init_script(src)?;
//! for call in state.drain_pending_host_calls() {
//!     match call.kind.as_str() {
//!         "Http.get" => {
//!             let body = my_http_client.get(&call.args[0])?;
//!             state.resolve_host_callback(call.id, Object::String(body.into()))?;
//!         }
//!         _ => {}
//!     }
//! }
//! ```

/// A queued async operation the VM is waiting on.
///
/// Hosts drain these after each VM function call and resolve them by
/// calling back with the awaited value. The VM treats `kind` and `args`
/// as opaque — it's the embedder's schema.
#[derive(Clone, Debug)]
pub struct PendingHostCall {
    /// Unique ID. Pass this back to
    /// [`crate::engine::ScriptState::resolve_host_callback`] so the VM
    /// can match the resume to the awaiting `yield`.
    pub id: u32,
    /// Host-interpreted kind string. Convention is `"Subsystem.verb"`
    /// (e.g. `"Http.get"`, `"Db.query"`), but nothing in the engine
    /// enforces that.
    pub kind: String,
    /// Serialised arguments. JSON strings or raw identifiers — the
    /// host decides. Stored as `Vec<String>` rather than an
    /// `Object`/`Value` to keep the structure trivially shippable
    /// across the WASM boundary.
    pub args: Vec<String>,
}
