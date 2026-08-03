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
    let stats = std::env::var_os("ZIPP_SHAPESTATS").is_some();
    let bstats = std::env::var_os("ZIPP_BUILTINSTATS").is_some();
    // Run the engine on a dedicated thread with a large stack. The interpreter
    // keeps JS recursion in an explicit frame stack, but every NATIVE re-entry
    // (a builtin callback, a generator/async resume, a direct eval, a JIT
    // bail-out) is a real Rust frame — and the main thread gets only 1 MiB on
    // Windows, which runaway recursion through such re-entries exhausts long
    // before the engine's own depth guards can raise a catchable RangeError.
    // 256 MiB of RESERVE (committed lazily by the OS) puts the engine's guards
    // firmly in charge.
    let r = std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(move || run(&args))
        .expect("spawn zipp worker thread")
        .join()
        .expect("zipp worker thread panicked");
    if stats {
        let (n, mx, tot) = zipp_vm::shape_stats();
        eprintln!("[shape] nodes={n} max_fanout={mx} edges={tot}");
    }
    if std::env::var_os("ZIPP_PROF").is_some() {
        let (rows, total) = zipp_vm::prof_stats();
        eprintln!("[prof] {total} samples @200us");
        for (name, n, pct) in &rows {
            eprintln!("[prof] {n:>8}  {pct:>5.1}%  {name}");
        }
    }
    if std::env::var_os("ZIPP_GCSTATS").is_some() {
        let (c, r, t, sw, re, slots, live, swept) = zipp_vm::gc_stats();
        eprintln!(
            "[gc] {c} collections  roots {r:.1}ms  trace {t:.1}ms  sweep {sw:.1}ms  retain {re:.1}ms  (total {:.1}ms)",
            r + t + sw + re
        );
        eprintln!("[gc] avg slots/collection {slots}  avg live {live}  total swept {swept}");
    }
    if std::env::var_os("ZIPP_RXSTATS").is_some() {
        let (compact, materialized) = zipp_vm::regexp_result_stats();
        let pct = if compact == 0 { 0.0 } else { materialized as f64 * 100.0 / compact as f64 };
        eprintln!(
            "[rx] {compact} compact match results  {materialized} materialized ({pct:.2}%)"
        );
    }
    if std::env::var_os("ZIPP_ASYNCSTATS").is_some() {
        let (reparks, reused, grew, values, subs_in, subs_sp) = zipp_vm::async_stats();
        let pct = if reparks == 0 { 0.0 } else { reused as f64 * 100.0 / reparks as f64 };
        eprintln!(
            "[async] {reparks} window re-parks  {reused} reused ({pct:.1}%)  {grew} grew  {values} values copied"
        );
        let subs = subs_in + subs_sp;
        let spct = if subs == 0 { 0.0 } else { subs_in as f64 * 100.0 / subs as f64 };
        eprintln!(
            "[async] {subs} promise subscriptions  {subs_in} inline ({spct:.1}%)  {subs_sp} spilled to a Vec"
        );
    }
    if std::env::var_os("ZIPP_ICSTATS").is_some() {
        let (gm, gk, gn, gd, sm, sg, sd, gah, sah) = zipp_vm::ic_stats();
        let pct = |x: u64, t: u64| if t == 0 { 0.0 } else { x as f64 * 100.0 / t as f64 };
        eprintln!(
            "[ic] GetProp misses {gm}  shape-known {gk} ({:.1}%)  shape-new {gn} ({:.1}%)  dict {gd} ({:.1}%)",
            pct(gk, gm), pct(gn, gm), pct(gd, gm)
        );
        eprintln!(
            "[ic] SetProp misses {sm}  guardable {sg} ({:.1}%)  dict {sd} ({:.1}%)",
            pct(sg, sm), pct(sd, sm)
        );
        eprintln!("[ic] accessor-way hits  get {gah}  set {sah}");
        let (cih, aih, hoh) = zipp_vm::call_inline_stats();
        eprintln!(
            "[ic] call/apply target inlines  call {cih}  apply {aih}  hasOwn-intrinsic {hoh}"
        );
        let (ins, ide) = zipp_vm::iter_region_stats();
        eprintln!("[ic] region iter-next  native steps {ins}  deopts {ide}");
    }
    if std::env::var_os("ZIPP_RXSTATS").is_some() {
        let (att, pushes, retries, elided, skips) = zipp_vm::rx_stats();
        eprintln!(
            "[rx] attempts {att}  greedy1 pushes {pushes}  retries {retries}  possessive elided {elided}  run skips {skips}"
        );
        let (jc, jdu, jdl, jn, jf, jb) = zipp_vm::rx_jit_stats();
        eprintln!(
            "[rx] jit compiled {jc}  declined unsupported {jdu}  declined limits {jdl}  native attempts {jn}  interp attempts {jf}  bails {jb}"
        );
    }
    if bstats {
        let rows = zipp_vm::builtin_stats();
        let total: u64 = rows.iter().map(|(_, _, c)| *c).sum();
        eprintln!("[builtin] {} distinct (kind, name) pairs, {total} calls", rows.len());
        for (kind, name, calls) in rows.iter().take(40) {
            let pct = if total == 0 { 0.0 } else { *calls as f64 * 100.0 / total as f64 };
            eprintln!("[builtin] {calls:>10}  {pct:>5.1}%  {kind}.{name}");
        }
    }
    match r {
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
            // `--script-goal` parses under the PURE Script goal: top-level
            // `return` is a SyntaxError and `import`/`export` are not accepted
            // (no Module-goal fallback). Off by default because node's CJS
            // wrapper makes both legal in real `.js` files; test262 wants the
            // grammar as specified, so its runner passes this.
            let mut path = None;
            for a in it.by_ref() {
                match a.as_str() {
                    "--script-goal" => zipp_vm::set_pure_script_goal(true),
                    other => {
                        path = Some(other.to_string());
                        break;
                    }
                }
            }
            let path = &path.ok_or("usage: zipp js [--script-goal] <file.js> [harness.js]")?;
            let src =
                std::fs::read_to_string(path).map_err(|e| format!("cannot read '{path}': {e}"))?;
            // An optional SECOND file is a harness prelude, exactly as `mjs`
            // takes one: it is evaluated as its own realm script before the
            // entry, which is what lets the entry's `"use strict"` directive
            // apply to the entry ALONE. test262's runner needs that — see
            // `run_with_harness`.
            let harness = it.next();
            // The script's directory resolves relative `import(specifier)` loads.
            let base_dir = std::path::Path::new(path).parent().map(|p| {
                if p.as_os_str().is_empty() {
                    std::path::Path::new(".").to_path_buf()
                } else {
                    p.to_path_buf()
                }
            });
            match harness {
                None => emit(zipp_vm::run_with_base(&src, base_dir)?),
                Some(h) => {
                    let hsrc = std::fs::read_to_string(h)
                        .map_err(|e| format!("cannot read harness '{h}': {e}"))?;
                    emit(zipp_vm::run_with_harness(&src, &hsrc, base_dir)?)
                }
            }
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
            // Parse with zipp's parser and print the AST.
            let mut path = None;
            let mut module = false;
            let mut sweep = false;
            for a in it.by_ref() {
                match a.as_str() {
                    "--module" => module = true,
                    "--sweep" => sweep = true,
                    // The front end is no longer selectable — there is one.
                    "--parser" => {}
                    other => path = Some(other.to_string()),
                }
            }
            let path = path.ok_or("usage: zipp ast <file-or-dir> [--module] [--sweep]")?;
            if !sweep {
                let src = std::fs::read_to_string(&path)
                    .map_err(|e| format!("cannot read '{path}': {e}"))?;
                println!("{}", zipp_vm::parse_to_text(&src, module)?);
                return Ok(());
            }
            // Sweep: parse everything and report failures grouped by reason.
            let mut files = Vec::new();
            collect_js(std::path::Path::new(&path), &mut files);
            files.sort();
            let (mut ok, mut failed) = (0usize, 0usize);
            let mut reasons: std::collections::BTreeMap<String, usize> = Default::default();
            for f in &files {
                let Ok(src) = std::fs::read_to_string(f) else { continue };
                let m = f.extension().is_some_and(|e| e == "mjs");
                let r = zipp_vm::parse_to_text(&src, m)
                    .or_else(|_| zipp_vm::parse_to_text(&src, true));
                match r {
                    Ok(_) => ok += 1,
                    Err(e) => {
                        failed += 1;
                        let key: String =
                            e.split(" (at ").next().unwrap_or(&e).chars().take(90).collect();
                        *reasons.entry(key).or_default() += 1;
                    }
                }
            }
            println!("{} files: {ok} parsed, {failed} failed", files.len());
            if !reasons.is_empty() {
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
        Some("--help") | Some("-h") | None => {
            println!("zipp — a clean-sheet JavaScript engine\n");
            println!("usage:");
            println!("  zipp js  <file.js>              run a script");
            println!("  zipp mjs <file.mjs>             run a file as an ES module");
            println!("  zipp bc  <file.js> [--module]   compile only, print the bytecode");
            println!("  zipp bcdiff <path>...           compile a corpus twice, diff the result");
            println!("  zipp ast <file-or-dir>          parse and print the AST (--sweep for a corpus)");
            println!("  zipp --version [--json]         build identity (commit, dirty digest, rustc, target)");
            println!("\nenvironment:");
            println!("  ZIPP_NOJIT=1                    interpreter only (no native codegen)");
            println!("  ZIPP_GC_STRESS=1                collect on every allocation");
            Ok(())
        }
        // Build identity. Exists because a benchmark artifact records the
        // executable's SHA-256 but nothing could say what that executable was
        // built FROM — so a stale binary silently passed an A/B gate (see
        // PERF_ROADMAP B61). `--json` is the form `tools/bench.py` embeds.
        Some("--version") | Some("-V") | Some("version") => {
            let json = it.next().map(|s| s.as_str()) == Some("--json");
            print!("{}", build_identity(json));
            Ok(())
        }
        Some(other) => Err(format!("unknown command '{other}' (try `zipp js <file.js>`)")),
    }
}

/// This binary's build identity, as human text or as one JSON object.
///
/// `source` is the field that matters for a benchmark: `<commit>` when the tree
/// was clean, `<commit>+dirty.<digest>` when it was not, where the digest covers
/// `git diff HEAD`. Two builds of different uncommitted edits therefore differ,
/// which a bare commit hash cannot express — and recording the parent commit for
/// a dirty build is precisely how a benchmark artifact came to name the wrong
/// source.
///
/// `#[cold]`/`#[inline(never)]` are not decoration. The release profile is fat
/// LTO with one codegen unit, so adding `format!` machinery anywhere reachable
/// from `main` perturbs hot-code placement: the first version of this function,
/// left inlinable, cost a REPLICATED ~1.5% on markdown-render and ~1.2% on
/// json-large with no runtime mechanism whatsoever. Keep it out of line.
#[cold]
#[inline(never)]
fn build_identity(json: bool) -> String {
    let commit = env!("ZIPP_BUILD_COMMIT");
    let dirty = env!("ZIPP_BUILD_DIRTY") == "true";
    let digest = env!("ZIPP_BUILD_DIFF_DIGEST");
    let source = if dirty { format!("{commit}+dirty.{digest}") } else { commit.to_string() };
    // Asked of the VM crate, not `cfg!` here: the `jit` feature belongs to
    // zipp-vm, so a local `cfg!` would report every build as interpreter-only.
    let jit = zipp_vm::jit_enabled();
    let features = env!("ZIPP_BUILD_FEATURES");
    let rustflags = env!("ZIPP_BUILD_RUSTFLAGS");
    if json {
        // Hand-rolled: the CLI has no JSON dependency and every value here is
        // either a digest, an identifier or a compiler version string.
        let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        return format!(
            concat!(
                "{{\"name\":\"zipp\",\"version\":\"{}\",\"source\":\"{}\",\"commit\":\"{}\",",
                "\"dirty\":{},\"diff_digest\":\"{}\",\"rustc\":\"{}\",\"target\":\"{}\",",
                "\"profile\":\"{}\",\"opt_level\":\"{}\",\"features\":\"{}\",",
                "\"rustflags\":\"{}\",\"jit\":{}}}\n"
            ),
            env!("CARGO_PKG_VERSION"),
            esc(&source),
            esc(commit),
            dirty,
            esc(digest),
            esc(env!("ZIPP_BUILD_RUSTC")),
            esc(env!("ZIPP_BUILD_TARGET")),
            esc(env!("ZIPP_BUILD_PROFILE")),
            esc(env!("ZIPP_BUILD_OPT_LEVEL")),
            esc(features),
            esc(rustflags),
            jit,
        );
    }
    let mut s = format!("zipp {}\n", env!("CARGO_PKG_VERSION"));
    s += &format!("source:    {source}\n");
    s += &format!("commit:    {commit}{}\n", if dirty { " (DIRTY)" } else { "" });
    s += &format!("rustc:     {}\n", env!("ZIPP_BUILD_RUSTC"));
    s += &format!("target:    {}\n", env!("ZIPP_BUILD_TARGET"));
    s += &format!(
        "profile:   {} (opt-level {})\n",
        env!("ZIPP_BUILD_PROFILE"),
        env!("ZIPP_BUILD_OPT_LEVEL")
    );
    s += &format!("features:  {}\n", if features.is_empty() { "(none)" } else { features });
    s += &format!("jit:       {}\n", if jit { "x86-64 native" } else { "disabled (interpreter only)" });
    if !rustflags.is_empty() {
        s += &format!("rustflags: {rustflags}\n");
    }
    s
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
