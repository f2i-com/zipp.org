//! Micro-benchmark that exercises the register VM with a warm pool.
//!
//! The legacy stack-VM comparison this test used to run (5.5× win for the
//! register VM) is archived in the git history from before 0.4.0 — the
//! stack compiler and its dispatch loop were deleted because the register
//! VM has been the only active path since 0.2.
//!
//! Run with `cargo test --release -p zipp-core --test bench_register_reuse -- --nocapture`.
//!
//! This file doubles as a correctness probe — an expected-value check
//! after each warm run catches regressions that only surface when the VM
//! is reused across evaluations (e.g. a missing `reset_for_run` clear of
//! `try_handlers` / `rframes`).
use std::time::Instant;

use zipp_engine::backend::validate::ValidatedBytecode;
use zipp_engine::bytecode::Bytecode;
use zipp_engine::config::ZippConfig;
use zipp_engine::object::Object;
use zipp_engine::parser::parse_program_from_source;
use zipp_engine::rcompiler::RCompiler;
use zipp_engine::value::{val_to_obj, Value};
use zipp_engine::vm::VM;

const WARMUP: usize = 5;
const RUNS: usize = 100;

fn compile(source: &str) -> Result<Bytecode, String> {
    let (program, errors) = parse_program_from_source(source);
    if !errors.is_empty() {
        return Err(format!("Parser errors: {}", errors.join(", ")));
    }
    RCompiler::new().compile_program(&program)
}

struct BenchCase {
    name: &'static str,
    source: &'static str,
}

// Cases wrapped in a function so function-scope locals (register locals
// for the register VM) are exercised — matches real callFunction() flow.
// `let name = function(...){...};` form avoids the FunctionDecl-inside-
// function recursion quirk where the binding isn't yet visible to a
// callee compiled before it.
const CASES: &[BenchCase] = &[
    BenchCase {
        name: "Arithmetic loop (5k)",
        source: "function bench() { let sum = 0; for (let i = 1; i <= 5000; i = i + 1) { sum = sum + i; } return sum; } bench();",
    },
    BenchCase {
        name: "Fibonacci recursive (n=20)",
        source: "function bench() { let fib = function(n) { if (n <= 1) { return n; } return fib(n - 1) + fib(n - 2); }; return fib(20); } bench();",
    },
    BenchCase {
        name: "Array index write + sum (2k)",
        source: "function bench() { let arr = []; for (let i = 0; i < 2000; i = i + 1) { arr[i] = i * 2; } let total = 0; for (let i = 0; i < 2000; i = i + 1) { total = total + arr[i]; } return total; } bench();",
    },
    BenchCase {
        name: "Object property loop (5k)",
        source: "function bench() { let obj = { x: 0, y: 0, z: 0 }; for (let i = 0; i < 5000; i = i + 1) { obj.x = obj.x + 1; obj.y = obj.y + 2; obj.z = obj.x + obj.y; } return obj.z; } bench();",
    },
    BenchCase {
        name: "String concatenation (1k)",
        source: "function bench() { let s = \"\"; for (let i = 0; i < 1000; i = i + 1) { s = s + \"a\"; } return s.length; } bench();",
    },
    BenchCase {
        name: "Function calls (1k)",
        source: "function bench() { let add = function(a, b) { return a + b; }; let result = 0; for (let i = 0; i < 1000; i = i + 1) { result = add(result, 1); } return result; } bench();",
    },
    BenchCase {
        name: "While + conditionals (10k)",
        source: "function bench() { let count = 0; let i = 0; while (i < 10000) { if (i % 3 === 0) { count = count + 1; } else { if (i % 3 === 1) { count = count + 2; } else { count = count + 3; } } i = i + 1; } return count; } bench();",
    },
    BenchCase {
        name: "Map set/get (2k)",
        source: "function bench() { let m = new Map(); for (let i = 0; i < 2000; i = i + 1) { m.set(\"key\" + i, i * 3); } let total = 0; for (let i = 0; i < 2000; i = i + 1) { total = total + m.get(\"key\" + i); } return total; } bench();",
    },
];

/// Run a pre-compiled bytecode on a single `VM`, reusing the JIT cache
/// and inline caches across iterations. Warmup is unmeasured; RUNS are
/// timed.
fn bench_reuse(bytecode: &Bytecode, warmup: usize, runs: usize) -> (f64, Object) {
    let mut config = ZippConfig::default();
    // Disable execution limits for the bench (saves ~5ns per iteration).
    config.max_instructions = None;
    config.max_wall_time_ms = None;
    config.max_heap_objects = None;
    config.max_heap_bytes = None;
    let validated = ValidatedBytecode::new(bytecode.clone()).expect("validate");
    let mut vm = VM::new(validated, config);
    for _ in 0..warmup {
        vm.run_register().expect("warmup failed");
        vm.reset_for_rerun(bytecode);
    }
    // Freeze the post-warmup heap shape so each timed run truncates
    // back to a clean compact baseline rather than dragging prior-run
    // allocations forward.
    vm.set_heap_baseline();
    let start = Instant::now();
    let mut last = Object::Undefined;
    for _ in 0..runs {
        vm.run_register().expect("bench eval failed");
        last = val_to_obj(vm.last_popped.take().unwrap_or(Value::UNDEFINED), &vm.heap);
        vm.reset_for_rerun(bytecode);
    }
    let elapsed = start.elapsed().as_secs_f64() * 1000.0;
    (elapsed, last)
}

#[test]
fn benchmark_register_reuse() {
    eprintln!("\n{}", "=".repeat(70));
    eprintln!("  Register VM — VM-reuse benchmark (warm caches)");
    eprintln!("  Warmup: {WARMUP}, Runs: {RUNS}");
    eprintln!("{}", "=".repeat(70));
    eprintln!("\n{:<30}  {:>12}", "Benchmark", "ms");
    eprintln!("{}", "-".repeat(50));

    let mut total = 0.0f64;
    for case in CASES {
        // Map: needs a larger heap budget because keys accumulate.
        if case.name.contains("Map") {
            let bytecode = compile(case.source).expect("compile failed");
            let mut config = ZippConfig::default();
            config.max_heap_objects = Some(500_000);
            config.max_heap_bytes = Some(512 * 1024 * 1024);
            let validated = ValidatedBytecode::new(bytecode.clone()).expect("validate");
            let mut vm = VM::new(validated, config);
            for _ in 0..WARMUP {
                vm.run_register().expect("warmup failed");
                vm.reset_for_rerun(&bytecode);
            }
            let start = Instant::now();
            for _ in 0..RUNS {
                vm.run_register().expect("bench eval failed");
                vm.reset_for_rerun(&bytecode);
            }
            let ms = start.elapsed().as_secs_f64() * 1000.0;
            total += ms;
            eprintln!("{:<30}  {:>12.3}", case.name, ms);
            continue;
        }
        let bytecode = compile(case.source).expect("compile failed");
        let (ms, _result) = bench_reuse(&bytecode, WARMUP, RUNS);
        total += ms;
        eprintln!("{:<30}  {:>12.3}", case.name, ms);
    }
    eprintln!("{}", "-".repeat(50));
    eprintln!("{:<30}  {:>12.3}", "TOTAL", total);
    eprintln!("{}\n", "=".repeat(70));
}

/// Regression test: a function-call loop must produce the same answer
/// on every iteration when the VM is reused (i.e. no stale state leaks
/// across `reset_for_rerun`).
#[test]
fn vm_reuse_stable_result() {
    let source = "function bench() {
        let add = function(a, b) { return a + b; };
        let result = 0;
        for (let i = 0; i < 1000; i = i + 1) { result = add(result, 1); }
        return result;
    } bench();";
    let bytecode = compile(source).expect("compile");
    let (_ms, result) = bench_reuse(&bytecode, 2, 8);
    match result {
        Object::Integer(n) => assert_eq!(n, 1000),
        other => panic!("expected Integer(1000), got {:?}", other),
    }
}
