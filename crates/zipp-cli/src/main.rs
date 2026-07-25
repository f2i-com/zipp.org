//! `zipp` — the command-line front end for the zipp-vm JavaScript engine.
//!
//! Usage:
//!   zipp js  <file.js>            run a script
//!   zipp mjs <file.mjs> [harness] run a file as an ES module

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("zipp: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Print an engine outcome the way `node` does — `console.log` to stdout,
/// `console.error`/`console.warn` to stderr — and surface a thrown error as a
/// process failure.
fn emit(outcome: zipp_vm::Outcome) -> Result<(), String> {
    for line in &outcome.output {
        println!("{line}");
    }
    for line in &outcome.errput {
        eprintln!("{line}");
    }
    match outcome.error {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

fn run(args: &[String]) -> Result<(), String> {
    let mut it = args.iter();
    let cmd = it.next().map(|s| s.as_str());
    match cmd {
        Some("js") | Some("js-vm") => {
            // Dynamic JavaScript engine (zipp-vm): a NaN-boxed, explicit-frame
            // register VM (recursion lives in an explicit frame stack, not the
            // native stack) with a native x86-64 OSR JIT. console.log streams to
            // stdout during eval, like `node file.js`. (`js` and `js-vm` are
            // aliases for the same engine.)
            let path = it.next().ok_or("usage: zipp js <file.js>")?;
            let src =
                std::fs::read_to_string(path).map_err(|e| format!("cannot read '{path}': {e}"))?;
            // The script's directory resolves relative `import(specifier)` loads.
            let base_dir = std::path::Path::new(path).parent().map(|p| {
                if p.as_os_str().is_empty() {
                    std::path::Path::new(".").to_path_buf()
                } else {
                    p.to_path_buf()
                }
            });
            emit(zipp_vm::run_with_base(&src, base_dir)?)
        }
        Some("mjs") => {
            // Run a file as an ES MODULE (top-level await; module-scoped
            // declarations; the event loop drains to completion). Used for
            // `flags:[module]` test262 tests and `.mjs` entry points.
            let path = it.next().ok_or("usage: zipp mjs <file.mjs>")?;
            let harness = it.next();
            emit(zipp_vm::run_module_file(
                std::path::Path::new(&path),
                harness.map(|s| s.as_str()),
            )?)
        }
        Some("--help") | Some("-h") | None => {
            println!("zipp — a clean-sheet JavaScript engine\n");
            println!("usage:");
            println!("  zipp js  <file.js>              run a script");
            println!("  zipp mjs <file.mjs>             run a file as an ES module");
            println!("\nenvironment:");
            println!("  ZIPP_NOJIT=1                    interpreter only (no native codegen)");
            println!("  ZIPP_GC_STRESS=1                collect on every allocation");
            Ok(())
        }
        Some(other) => Err(format!("unknown command '{other}' (try `zipp js <file.js>`)")),
    }
}
