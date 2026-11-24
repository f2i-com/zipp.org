# Rust on ZIPP

ZIPP compiles Rust scripts, libraries, and multi-file Cargo backend applications directly into its shared register bytecode and runs them on the same virtual machine and native x86-64/ARM64 JIT as JavaScript, TypeScript, Python, and PHP — with zero external `rustc`/`cargo` invocation and instant startup.

## Running Rust

```sh
# Run a Rust script directly (with CLI args passed to std::env::args())
zipp rs examples/rust/hello.rs [args...]
zipp examples/rust/hello.rs [args...]
zipp run --lang=rust examples/rust/hello.rs

# Run a Cargo project directory directly (auto-resolves Cargo.toml, src/lib.rs, src/main.rs)
zipp rs examples/rust/issue_tracker
zipp examples/rust/issue_tracker

# Run #[test] (sync and async) functions on a file or Cargo project directory
zipp rs --test examples/rust/hello.rs
zipp rs --test examples/rust/issue_tracker

# Inspect compiled ZIPP register bytecode
zipp rs --bc examples/rust/hello.rs

# Run a multi-threaded HTTP/1.1 backend worker server (boots router() once per worker VM)
zipp rs --serve 127.0.0.1:8080 examples/rust/issue_tracker
zipp rs --serve 127.0.0.1:8080 examples/rust/backend_api/server.rs

# Pipe Rust source via stdin
echo 'fn main() { println!("hi from stdin"); }' | zipp --lang=rs -
```

## Full-Stack Example Application

[`examples/rust/issue_tracker`](../examples/rust/issue_tracker) is a complete multi-file Cargo Rust web application that runs directly on the ZIPP VM:

- **Cargo layout**: `Cargo.toml`, `src/lib.rs`, `src/main.rs`, `src/{config,error,auth,store}.rs`, `src/models/{mod,user,issue}.rs`, `src/routes/{mod,auth,issues}.rs`, and static frontend assets under `public/{index.html,style.css}`.
- **Features exercised end-to-end**:
  - `Cargo.toml` metadata (`env!("CARGO_PKG_NAME")`, `env!("CARGO_PKG_VERSION")`) and sibling `src/lib.rs` crate imports (`use issue_tracker::...;`) + `pub use` named/glob re-exports.
  - `async fn` HTTP route handlers returning `Result<Response, AppError>` with automatic `?` `From` conversion and custom `AppError::into_response()`.
  - `Router::nest`, `Router::serve_dir`, `Router::serve_file`, and request-ID / timing middleware.
  - Salted PBKDF-HMAC-SHA256 password hashing (`crypto::hash_password`/`verify_password`) and HS256 JWT authentication (`crypto::jwt_sign`/`jwt_verify` + `req.bearer_token()`).
  - `#[serde(rename_all = "camelCase")]`, `#[serde(rename_all = "snake_case")]`, `#[serde(skip_serializing)]`, `#[serde(skip_serializing_if = "Option::is_none")]`, and nested generic JSON persistence (`StoreSnapshot` with `Vec<UserRecord>` and `Vec<Issue>`).
  - In-place `&mut self` enum state machine transitions (`issue.status.transition_to(...)`).
  - `#[cfg(test)] mod tests` with both sync and `#[test] async fn` unit tests (`zipp rs --test examples/rust/issue_tracker`).

## Architecture

| Path | Role |
| --- | --- |
| [`crates/zipp-vm/src/lang/rust/parser`](../crates/zipp-vm/src/lang/rust/parser) | `zipp-rsparse`: pure-Rust (`#![forbid(unsafe_code)]`, zero-dependency) Rust 2021/2024 lexer, AST, and recursive-descent/Pratt parser. |
| [`crates/zipp-vm/src/lang/rust/frontend`](../crates/zipp-vm/src/lang/rust/frontend) | `Cargo.toml` + `src/lib.rs` discovery, multi-file `mod foo;` / `#[path = "..."]` loader, `use` & cross-module `pub use` (named & glob) resolver, `#[cfg(test)]` filtering, `macro_rules!` & built-in macro expander, static numeric & target-type hint inference, direct ZIPP register bytecode emitter, and persistent disk runtime cache. |
| [`crates/zipp-vm/src/lang/rust/runtime`](../crates/zipp-vm/src/lang/rust/runtime) | Rust runtime (`__zipp_rs`), compiled once into a memoized & disk-cached `RuntimeSeed`: structs, enums, tuples, `String`/`&str` (UTF-8 byte lengths), `char`, `&mut` ref cells (`*self = ...`), boxed closures (`Box<dyn Fn>`), `From`/`TryFrom`/`Into`/`TryInto`, `Vec`/slices, lazy `Iterator` adapters/consumers (including custom `impl Iterator`), `HashMap`/`BTreeMap` (with `Entry` API), `HashSet`/`BTreeSet`, `VecDeque`, `BinaryHeap`, `Option`, `Result`, `Arc`/`Mutex`/`RwLock`/`RefCell`/`Cell`/`OnceLock`/`Atomic*`, `BufReader`/`Cursor`, `Utc::now()`, `html::escape`, `serde_json` (with `#[serde(...)]` attributes & nested generics), `regex`, `crypto`, `zipp::kv`, and `http::Router`/`Request`/`Response`/`StatusCode`/`Client`. |
| [`crates/zipp-vm/src/lang/rust/vm`](../crates/zipp-vm/src/lang/rust/vm) | Private VM natives (`__zipp_rs_io`, `__zipp_rs_fs`, `__zipp_rs_env`, `__zipp_rs_time`, `__zipp_rs_net`, `__zipp_rs_crypto`, `__zipp_rs_proc`) bound by slot only in Rust programs and strictly gated under `safe-sandbox` / `wasm32`. |
| [`crates/zipp-cli/src/rust_server.rs`](../crates/zipp-cli/src/rust_server.rs) | Multi-threaded native HTTP/1.1 worker pool for `zipp rs --serve`. |

## What Runs

- **Core Rust Language:** `fn`, `async fn` / `async { ... }` / `.await` (using ZIPP's native Promise/microtask engine + synchronous `block_on` bridge for async HTTP handlers and `#[test] async fn`), structs (named, tuple, unit, `..rest` update), enums (unit, tuple, struct variants, explicit discriminants), `impl` & `trait` (including default trait methods, `Self`, `&mut self` with `*self = ...` on locals, fields, and indices, custom `Iterator`, multi-impl `From`/`TryFrom`), `#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize)]`, `#[serde(...)]` (`rename_all`, `rename`, `alias`, `skip`, `skip_serializing`, `skip_deserializing`, `skip_serializing_if = "Option::is_none"`, `flatten`), `#[cfg(test)]` / `#[cfg(not(test))]`, variable shadowing, `&mut` mutable references & `*` deref, closures (`|x| ...`, `move |x| ...`, mutable captures, `Box<dyn Fn>`), full pattern matching (`match`, guards, `|`, `@`, ranges `1..=5`, slices `[a, mid @ .., z]`, `if let` + let-chains `&&`, `while let`, `let ... else`), labeled loops & `break 'label val`, `?` try operator with automatic `From` error conversion, `as` numeric/char casts, multi-file `mod foo;`, `Cargo.toml` + `src/lib.rs`, and `use` / `pub use` re-exports.
- **Macros:** `println!`, `print!`, `eprintln!`, `eprint!`, `format!` (full Rust `std::fmt` specifiers + inline `{var}` capture), `write!`, `writeln!`, `vec!`, `assert!`, `assert_eq!`, `assert_ne!`, `debug_assert!*`, `panic!`, `todo!`, `unimplemented!`, `unreachable!`, `dbg!`, `matches!`, `json!`, `concat!`, `stringify!`, `include_str!`, `include_bytes!`, `env!` (including `CARGO_PKG_NAME`, `CARGO_PKG_VERSION`, `CARGO_MANIFEST_DIR`, `CARGO_CRATE_NAME`), `option_env!`, `file!`, `line!`, `module_path!`, `cfg!`, `tokio::join!`, `info!`/`warn!`/`error!`/`debug!`/`trace!`, `anyhow!`, `bail!`, `ensure!`, plus declarative `macro_rules!` expansion (including `$(...)*` repetitions).
- **Backend & Scripting Stdlib:** `std::io` (`BufReader`, `Cursor`, `stdout`/`stderr`/`stdin`), `std::fs`, `std::path::Path`/`PathBuf`, `std::env`, `std::process::Command`/`exit`, `std::time::Instant`/`SystemTime`/`Duration`, `Utc::now()` (RFC-3339 / timestamps), `std::thread::sleep`, `std::net::TcpListener`/`TcpStream`, `std::sync::{Arc, Mutex, RwLock, OnceLock, LazyLock, atomic::*}`, `std::cell::{Cell, RefCell}`, `std::mem`, `std::cmp`, `std::iter`, `std::panic::catch_unwind`, `serde_json`, `regex::Regex`, `html::escape`, `crypto` (SHA-256, SHA-512, HMAC-SHA256, MD5, Base64/Base64URL, Hex, UUID v4, random bytes, PBKDF-HMAC-SHA256 password hashing, HS256 JWT sign/verify), `zipp::kv::Store`, and built-in `http::{Router, Request, Response, StatusCode, Client}` (`nest`, `merge`, `serve_dir`, `serve_file`, `layer`, `with_state`, `bearer_token`, `cookie`, `form`, `IntoResponse`).
- **Native JIT Acceleration & O(1) Collections:** User Rust functions (`__rs_fn_*`) and integer `for i in a..b` / `a..=b` loops lower directly to unboxed ZIPP register bytecode (`LoadInt`, `Add`, `Sub`, `Mul`, `Lt`, `Le`, `AddInt`, `Call`, `Return`) and compile natively on ZIPP's x86-64 Tier-A and Tier-C JITs while the JS runtime stays interpreted. From v0.0.40, `VecDeque`, `BTreeMap`, and `String::pop` use O(1) ring-buffer and cached-order operations (8.6× faster, `481 ms → 56 ms`).

## Feature Flag & Sandbox Isolation

Rust support is behind the optional `rust` Cargo feature on `zipp-vm`, `zipp-cli`, and `zipp-wasm` (excluded from `wasm-lite`). Under `safe-sandbox` and `wasm32`, native host filesystem, process execution, and TCP networking are compiled out; callers can supply an in-memory binary-safe VFS via `RustProgramConfig::vfs` and host SQL bridges via `addAppBridge("sql", ...)`. Try live single- and multi-file Rust examples directly in the [browser playground](https://www.zipp.org/playground/).

## Validation

```sh
cargo test -p zipp-rsparse
cargo test -p zipp-vm --features rust --test rust_language --test rust_backend
cargo test -p zipp-cli --test rust_cli_and_server
```
