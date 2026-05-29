//! `zipp` — the ZIPP CLI.
//!
//! Usage:
//!   zipp run [--prove] <file.zipp>
//!
//! `--prove` (requires the `zk` feature, on by default) re-runs with trace
//! recording, generates a zk-STARK proof of the execution, and verifies it.

use std::process::ExitCode;
use std::time::Instant;

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

fn run(args: &[String]) -> Result<(), String> {
    let mut it = args.iter();
    let cmd = it.next().map(|s| s.as_str());
    match cmd {
        Some("run") => {
            let mut prove = false;
            let mut jit = false;
            let mut fast_math = false;
            let mut path: Option<String> = None;
            for a in it {
                match a.as_str() {
                    "--prove" => prove = true,
                    "--jit" => jit = true,
                    "--ffast-math" => fast_math = true,
                    s if s.starts_with("--") => return Err(format!("unknown flag '{s}'")),
                    s => path = Some(s.to_string()),
                }
            }
            let path = path.ok_or("usage: zipp run [--prove] [--jit [--ffast-math]] <file.zipp>")?;
            if fast_math && !jit {
                return Err("--ffast-math only applies with --jit".into());
            }
            run_file(&path, prove, jit, fast_math)
        }
        Some("--help") | Some("-h") | None => {
            println!("ZIPP v0 — sound-TS-subset language (PLAN.md)\n");
            println!("usage:");
            println!("  zipp run <file.zipp>            compile + run");
            println!("  zipp run --prove <file.zipp>    run, then zk-STARK prove + verify the execution");
            Ok(())
        }
        Some(other) => Err(format!("unknown command '{other}' (try `zipp run <file.zipp>`)")),
    }
}

fn run_file(path: &str, prove: bool, jit: bool, fast_math: bool) -> Result<(), String> {
    let src = std::fs::read_to_string(path).map_err(|e| format!("cannot read '{path}': {e}"))?;
    let program = zippc::compile(&src)?;

    if jit && prove {
        return Err("--jit and --prove can't be combined (the prover needs the interpreter trace)".into());
    }
    if jit {
        if let Some((r, compile, execute)) = jit_run(&program, fast_math)? {
            let fm = if fast_math { " · ffast-math" } else { "" };
            println!(
                "=> {r} (jit · native{fm} · compiled in {compile:.2?}, ran in {execute:.2?})"
            );
            return Ok(());
        }
        // ineligible — jit_run explained why; fall through to the interpreter.
    }

    let t0 = Instant::now();
    let result = zippc::vm::run(&program, prove)?;
    let elapsed = t0.elapsed();

    for v in &result.output {
        println!("{v}");
    }
    println!("=> {} ({} fns, ran in {:.2?})", result.result, program.funcs.len(), elapsed);

    if prove {
        let r = result
            .result
            .as_i64()
            .ok_or("internal: prove produced a non-integer result")?;
        prove_and_verify(&result.trace, r, hash_source(&src))?;
    }
    Ok(())
}

/// Deterministic FNV-1a hash of the source — binds a proof to a program.
fn hash_source(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01B3);
    }
    h
}

// Returns (formatted result, compile time, execute time) so the non-jit stub
// below shares one signature without naming a zipp_jit type. Compile and
// execute are reported separately — the generated code's real speed is the
// execute half; compile is one-shot AOT cost.
#[cfg(feature = "jit")]
type JitTimed = (String, std::time::Duration, std::time::Duration);

#[cfg(feature = "jit")]
fn jit_run(program: &zippc::Program, fast_math: bool) -> Result<Option<JitTimed>, String> {
    if let Some(bad) = zipp_jit::ineligible_reason(program) {
        eprintln!("zipp: --jit covers the scalar subset only (program uses {bad}); using the interpreter");
        return Ok(None);
    }
    let o = zipp_jit::run_with(program, fast_math)?;
    Ok(Some((o.value.to_string(), o.compile, o.execute)))
}

#[cfg(not(feature = "jit"))]
fn jit_run(
    _program: &zippc::Program,
    _fast_math: bool,
) -> Result<Option<(String, std::time::Duration, std::time::Duration)>, String> {
    Err("this build has no jit profile — rebuild with the `jit` feature".into())
}

#[cfg(feature = "zk")]
fn prove_and_verify(trace: &[zippc::TraceStep], result: i64, program_hash: u64) -> Result<(), String> {
    if trace.is_empty() {
        return Err("nothing to prove (empty trace)".into());
    }
    let arithmetic = trace
        .iter()
        .filter(|s| !matches!(s.op, zippc::OpKind::Other))
        .count();
    println!(
        "\n── zk-STARK profile ──\ntrace: {} steps ({} arithmetic constrained)",
        trace.len(),
        arithmetic
    );

    // Winterfell's per-constraint degree check is a debug_assert; in a debug
    // build, traces that don't exercise every opcode class trip it. Catch that
    // and advise a release build instead of dumping a panic + backtrace.
    let t0 = Instant::now();
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let proved = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        zipp_zk::prove(trace, result, program_hash)
    }));
    std::panic::set_hook(prev_hook);
    let (proof, pub_inputs) = match proved {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => return Err(e),
        Err(_) => {
            return Err("zk proving hit a debug-only assertion — run a release build: \
                        `cargo build --release` then `./target/release/zipp run --prove ...`"
                .into())
        }
    };
    let prove_t = t0.elapsed();
    let bytes = proof.to_bytes();
    println!("proved in {:.2?}, proof size {} bytes", prove_t, bytes.len());

    let t1 = Instant::now();
    zipp_zk::verify(&bytes, &pub_inputs)?;
    println!(
        "verified in {:.2?}  ✓ result {} is STARK-proven, bound to program {:#018x}",
        t1.elapsed(),
        pub_inputs.result,
        pub_inputs.program_hash
    );
    Ok(())
}

#[cfg(not(feature = "zk"))]
fn prove_and_verify(_trace: &[zippc::TraceStep], _result: i64, _program_hash: u64) -> Result<(), String> {
    Err("this build has no zk profile — rebuild with the `zk` feature".into())
}
