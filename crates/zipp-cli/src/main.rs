//! `zipp` — the command-line front end for the zipp-vm JavaScript engine.
//!
//! Usage:
//!   zipp js  <file.js>            run a script
//!   zipp mjs <file.mjs> [harness] run a file as an ES module

use std::process::ExitCode;

/// The engine is allocation-bound: every object owns three parallel `Vec`s plus
/// a `String` per property, so constructing `{a: 1}` costs four allocations.
/// Measured against the platform allocator that was ~290ns for the first
/// property alone — roughly 72ns per malloc, which is the allocator, not the
/// data structure.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

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
        Some("bc") => {
            // Compile only, print the canonical Program. No VM is constructed,
            // so this works on a script that would loop or throw.
            let mut path = None;
            let mut module = false;
            for a in it {
                match a.as_str() {
                    "--module" => module = true,
                    other => path = Some(other.to_string()),
                }
            }
            let path = path.ok_or("usage: zipp bc <file.js> [--module]")?;
            let src =
                std::fs::read_to_string(&path).map_err(|e| format!("cannot read '{path}': {e}"))?;
            println!("{}", zipp_vm::compile_to_text(&src, module)?);
            Ok(())
        }
        Some("ast") => {
            // Parse with oxc, lower to zipp's AST, print it. The bridge's
            // debugging aid and the input side of the Stage 1 gate.
            let mut path = None;
            let mut module = false;
            let mut sweep = false;
            let mut use_parser = false;
            for a in it.by_ref() {
                match a.as_str() {
                    "--module" => module = true,
                    "--sweep" => sweep = true,
                    "--parser" => use_parser = true,
                    other => path = Some(other.to_string()),
                }
            }
            let path = path.ok_or("usage: zipp ast <file-or-dir> [--module] [--sweep]")?;
            if !sweep {
                let src = std::fs::read_to_string(&path)
                    .map_err(|e| format!("cannot read '{path}': {e}"))?;
                let text = if use_parser {
                    zipp_vm::parse_to_text(&src, module)?
                } else {
                    zipp_vm::lower_to_text(&src, module)?
                };
                println!("{text}");
                return Ok(());
            }
            // Sweep: lower everything and report what the AST cannot yet say.
            let mut files = Vec::new();
            collect_js(std::path::Path::new(&path), &mut files);
            files.sort();
            let (mut ok, mut unsupported, mut unparsed) = (0usize, 0usize, 0usize);
            let mut reasons: std::collections::BTreeMap<String, usize> = Default::default();
            for f in &files {
                let Ok(src) = std::fs::read_to_string(f) else { continue };
                let m = f.extension().is_some_and(|e| e == "mjs");
                match zipp_vm::lower_to_text(&src, m) {
                    Ok(_) => ok += 1,
                    Err(e) if e.starts_with("SyntaxError") => unparsed += 1,
                    Err(e) => {
                        unsupported += 1;
                        // Group by reason, not by file: one gap usually explains many.
                        let key = e.splitn(2, " at ").next().unwrap_or(&e).to_string();
                        *reasons.entry(key).or_default() += 1;
                    }
                }
            }
            println!("{} files: {ok} lowered, {unsupported} unsupported, {unparsed} not valid JS", files.len());
            if !reasons.is_empty() {
                println!("
unsupported by reason:");
                let mut v: Vec<_> = reasons.into_iter().collect();
                v.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
                for (r, n) in v.iter().take(25) {
                    println!("  {n:5}  {r}");
                }
            }
            Ok(())
        }
        Some("bcdiff") => {
            // The differential gate. Compiles every file in the corpus TWICE
            // and compares the canonical Programs.
            //
            // Today both compiles go through the same front end, so this
            // measures determinism — which is the precondition for the gate,
            // and was violated until recently (the same source produced
            // different bytecode on every run). When a second front end lands,
            // only the second call changes, and the same command becomes the
            // parity gate.
            let paths: Vec<String> = it.cloned().collect();
            if paths.is_empty() {
                return Err("usage: zipp bcdiff <file-or-dir>...".into());
            }
            let mut files = Vec::new();
            for p in &paths {
                collect_js(std::path::Path::new(p), &mut files);
            }
            files.sort();
            let (mut same, mut differ, mut rejected) = (0usize, 0usize, 0usize);
            for f in &files {
                let Ok(src) = std::fs::read_to_string(f) else { continue };
                let module = f.extension().is_some_and(|e| e == "mjs");
                let a = zipp_vm::compile_to_text(&src, module);
                let b = zipp_vm::compile_to_text(&src, module);
                match (a, b) {
                    (Ok(a), Ok(b)) if a == b => same += 1,
                    (Ok(a), Ok(b)) => {
                        differ += 1;
                        let at = a
                            .lines()
                            .zip(b.lines())
                            .position(|(x, y)| x != y)
                            .map(|n| n + 1)
                            .unwrap_or(0);
                        println!("DIFFER  {}  (first at line {at})", f.display());
                    }
                    // A file both front ends reject the same way is not a
                    // mismatch — it is agreement about invalid input.
                    (Err(a), Err(b)) if a == b => rejected += 1,
                    (a, b) => {
                        differ += 1;
                        println!(
                            "DIFFER  {}  accept/reject: {:?} vs {:?}",
                            f.display(),
                            a.err(),
                            b.err()
                        );
                    }
                }
            }
            println!(
                "\n{} files: {same} identical, {rejected} rejected by both, {differ} MISMATCH",
                files.len()
            );
            if differ == 0 {
                println!("BCDIFF_OK=1");
                Ok(())
            } else {
                Err(format!("{differ} mismatches"))
            }
        }
        Some("parsediff") => {
            // The parser's gate: our parser vs the oxc bridge, over a corpus.
            // The bridge's output is already known to compile byte-identically,
            // so any disagreement here is the parser's, with one suspect.
            let paths: Vec<String> = it.cloned().collect();
            if paths.is_empty() {
                return Err("usage: zipp parsediff <file-or-dir>...".into());
            }
            let mut files = Vec::new();
            for p in &paths {
                collect_js(std::path::Path::new(p), &mut files);
            }
            files.sort();
            let (mut same, mut differ, mut ours_err, mut both_err) = (0usize, 0usize, 0usize, 0usize);
            let mut reasons: std::collections::BTreeMap<String, usize> = Default::default();
            for f in &files {
                let Ok(src) = std::fs::read_to_string(f) else { continue };
                let m = f.extension().is_some_and(|e| e == "mjs");
                let mut theirs = zipp_vm::lower_to_text(&src, m);
                let mut ours = zipp_vm::parse_to_text(&src, m);
                // A `.js` file that is really an ES MODULE: oxc parses module
                // syntax under any goal, so the bridge accepts it as a script
                // while our parser (correctly) rejects `import`/`export` there.
                // Retry BOTH sides under the module goal and compare those.
                if !m && ours.is_err() && theirs.is_ok() {
                    let mt = zipp_vm::lower_to_text(&src, true);
                    let mo = zipp_vm::parse_to_text(&src, true);
                    if mo.is_ok() {
                        theirs = mt;
                        ours = mo;
                    }
                }
                match (&theirs, &ours) {
                    (Ok(a), Ok(b)) if a == b => same += 1,
                    (Ok(_), Ok(_)) => {
                        differ += 1;
                        if differ <= 10 {
                            println!("DIFFER  {}", f.display());
                        }
                    }
                    // oxc rejected it too: agreement about invalid input.
                    (Err(_), Err(_)) => both_err += 1,
                    (Ok(_), Err(e)) => {
                        ours_err += 1;
                        let key: String = e.split(" (at ").next().unwrap_or(e).chars().take(90).collect();
                        *reasons.entry(key).or_default() += 1;
                    }
                    (Err(_), Ok(_)) => {
                        differ += 1;
                        if differ <= 10 {
                            println!("WE-ACCEPT-THEY-REJECT  {}", f.display());
                        }
                    }
                }
            }
            println!(
                "
{} files: {same} identical, {differ} differing AST, {ours_err} we reject, {both_err} both reject",
                files.len()
            );
            if !reasons.is_empty() {
                println!("
our parse failures by reason:");
                let mut v: Vec<_> = reasons.into_iter().collect();
                v.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
                for (r, n) in v.iter().take(20) {
                    println!("  {n:5}  {r}");
                }
            }
            Ok(())
        }
        Some("--help") | Some("-h") | None => {
            println!("zipp — a clean-sheet JavaScript engine\n");
            println!("usage:");
            println!("  zipp js  <file.js>              run a script");
            println!("  zipp mjs <file.mjs>             run a file as an ES module");
            println!("  zipp bc  <file.js> [--module]   compile only, print the bytecode");
            println!("  zipp bcdiff <path>...           compile a corpus twice, diff the result");
            println!("\nenvironment:");
            println!("  ZIPP_NOJIT=1                    interpreter only (no native codegen)");
            println!("  ZIPP_GC_STRESS=1                collect on every allocation");
            Ok(())
        }
        Some(other) => Err(format!("unknown command '{other}' (try `zipp js <file.js>`)")),
    }
}

/// Every `.js`/`.mjs` file under `p` (or `p` itself if it is a file).
fn collect_js(p: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    if p.is_file() {
        out.push(p.to_path_buf());
        return;
    }
    let Ok(entries) = std::fs::read_dir(p) else { return };
    for e in entries.flatten() {
        let path = e.path();
        if path.is_dir() {
            collect_js(&path, out);
        } else if path.extension().is_some_and(|x| x == "js" || x == "mjs") {
            out.push(path);
        }
    }
}
