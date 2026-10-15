//! Run a classic script through the embedding API in the safe-sandbox profile,
//! the way an embedder does, for speed comparisons with other engines.
//!
//! `cargo build --locked --release --example embed_bench` (from this crate);
//! then `embed_bench FILE.js`. A step budget (unlimited) and a 1 GiB heap
//! ceiling are attached unless `--no-limits` is given, so the recorder's
//! per-instruction check is in the measurement, as in an embedding that sets
//! limits; `--steps-only` and `--heap-only` attach one of the two. Console
//! output goes to stdout as it is produced. With `ZIPP_HEAPBYTES` set, the
//! heap figure's peak is printed to stderr at the end. The 0.0.25 release
//! record (docs/validation/2026-10-15-release-0.0.25.md) measured with it.

#![forbid(unsafe_code)]

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::thread::Builder::new()
        .name("embed-bench".into())
        .stack_size(256 * 1024 * 1024)
        .spawn(move || run(&args))
        .expect("spawn the engine thread")
        .join()
        .expect("engine thread panicked")
}

fn run(args: &[String]) -> ExitCode {
    let Some(path) = args.iter().find(|a| !a.starts_with("--")) else {
        eprintln!("usage: embed_bench FILE.js [--no-limits]");
        return ExitCode::from(2);
    };
    let limits = !args.iter().any(|a| a == "--no-limits");
    let steps = limits && !args.iter().any(|a| a == "--heap-only");
    let heap = limits && !args.iter().any(|a| a == "--steps-only");
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("embed_bench: {path}: {e}");
            return ExitCode::from(2);
        }
    };
    let mut state = match zipp_vm::embed::compile_script(&src) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    state.set_console_sink(Box::new(|stream, line| match stream {
        zipp_vm::embed::ConsoleStream::Stderr => eprintln!("{line}"),
        _ => println!("{line}"),
    }));
    if steps {
        state.set_limits(u64::MAX, None);
    }
    if heap {
        state.set_heap_limit(1 << 30);
    }
    let result = state.run_init();
    state.run_microtasks();
    if std::env::var_os("ZIPP_HEAPBYTES").is_some() {
        eprintln!("heap_bytes peak={}MB", state.heap_bytes() >> 20);
    }
    match result {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
