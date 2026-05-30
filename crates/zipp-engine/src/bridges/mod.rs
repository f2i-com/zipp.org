//! Host integration bridges.
//!
//! Each bridge defines a trait (and usually helper types) that the embedder
//! implements to expose host capabilities to running scripts. The VM calls
//! into these traits synchronously on the hot path, or asynchronously via
//! the [`host_bridge::PendingHostCall`] yield protocol.
//!
//! Module names are kept snake-case `*_bridge` to match the stable public
//! surface used by the WASM wrapper and downstream embedders.
//!
//! | Module            | Responsibility                                      |
//! |-------------------|-----------------------------------------------------|
//! | [`host_bridge`]   | Async host-call queue (`PendingHostCall`).          |
//! | [`db_bridge`]     | CRUD + sync operations over an embedder-defined DB. |
//! | [`draw_bridge`]   | Canvas-style drawing commands.                      |
//! | [`env_bridge`]    | Environment variable access.                        |
//! | [`fs_bridge`]     | Virtual filesystem access.                          |
//! | [`http_bridge`]   | Outgoing HTTP requests.                             |
//! | [`input_bridge`]  | UI input events.                                    |
//! | [`layout_bridge`] | DOM / layout queries.                               |
//! | [`local_storage`] | Key/value persistence.                              |

pub mod db_bridge;
pub mod draw_bridge;
pub mod env_bridge;
pub mod fs_bridge;
pub mod host_bridge;
pub mod http_bridge;
pub mod input_bridge;
pub mod layout_bridge;
pub mod local_storage;
