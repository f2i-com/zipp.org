//! Node's globals, for a program the `zipp` command runs from a file.
//!
//! A program written for Node expects more than ECMAScript: `process`,
//! `global`, `structuredClone`, `queueMicrotask`, timers with ids that
//! `clearTimeout` cancels. They are written in JavaScript (`bootstrap.js`),
//! as Python's runtime is, over a handful of host functions here: the
//! process's environment, arguments and working directory, writes to its
//! output streams, a clock, and exit.
//!
//! None of it costs a program that does not use it. The bootstrap only
//! defines accessors on the global object; the first read of one builds
//! that global and replaces the accessor with it, as a data property a
//! program can assign over.
//!
//! The layer is the command's, switched on per process ([`set_enabled`])
//! for a file run without a harness. An embedder, the WebAssembly engines,
//! the sandbox and the test262 runner (which always passes a harness) get
//! the plain ECMAScript global object.
//!
//! All of it is the `node` feature's, but for [`resolve`], which the ES
//! module loader uses in every build.

pub(crate) mod resolve;

#[cfg(feature = "node")]
mod packages;
#[cfg(feature = "node")]
mod runtime;
#[cfg(feature = "node")]
pub(crate) use runtime::*;
