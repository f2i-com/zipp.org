//! `zipp-sandbox` — hardened native runner for untrusted classic scripts.

#![forbid(unsafe_code)]

use std::process::ExitCode;

// The native JIT CLI and this no-JIT executable deliberately share one audited
// supervisor/worker implementation. The separate Cargo workspaces, rather
// than separate source copies, provide the dependency-feature boundary.
#[allow(dead_code)] // the shared module also exposes the integrated-CLI entry
#[path = "../../zipp-cli/src/sandbox.rs"]
mod sandbox;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = std::thread::Builder::new()
        .name("zipp-sandbox-engine".into())
        // Native builtins can re-enter the VM on the Rust stack. Reserve a
        // large, lazily committed stack so zipp-vm's explicit depth guards
        // remain authoritative on platforms whose main thread is small.
        .stack_size(256 * 1024 * 1024)
        .spawn(move || dispatch(&args))
        .expect("spawn zipp-sandbox engine thread")
        .join()
        .expect("zipp-sandbox engine thread panicked");

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("zipp-sandbox: {error}");
            ExitCode::FAILURE
        }
    }
}

fn dispatch(args: &[String]) -> Result<(), String> {
    match args.split_first() {
        Some((command, rest)) if command == "__sandbox-child" => sandbox::run_child(rest),
        _ => sandbox::run_standalone(args),
    }
}
