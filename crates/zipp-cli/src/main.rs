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
            let mut path: Option<String> = None;
            for a in it {
                match a.as_str() {
                    "--prove" => prove = true,
                    s if s.starts_with("--") => return Err(format!("unknown flag '{s}'")),
                    s => path = Some(s.to_string()),
                }
            }
            let path = path.ok_or("usage: zipp run [--prove] <file.zipp>")?;
            run_file(&path, prove)
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

fn run_file(path: &str, prove: bool) -> Result<(), String> {
    let src = std::fs::read_to_string(path).map_err(|e| format!("cannot read '{path}': {e}"))?;
    let program = zippc::compile(&src)?;

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
        prove_and_verify(&result.trace, r)?;
    }
    Ok(())
}

#[cfg(feature = "zk")]
fn prove_and_verify(trace: &[zippc::TraceStep], result: i64) -> Result<(), String> {
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
        zipp_zk::prove(trace, result)
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
    println!("verified in {:.2?}  ✓ execution result {} is STARK-proven", t1.elapsed(), pub_inputs.result);
    Ok(())
}

#[cfg(not(feature = "zk"))]
fn prove_and_verify(_trace: &[zippc::TraceStep], _result: i64) -> Result<(), String> {
    Err("this build has no zk profile — rebuild with the `zk` feature".into())
}
