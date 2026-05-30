//! End-to-end smoke tests against the public [`ZippEngine`] surface.
//!
//! These are integration tests — they link against the crate's public API
//! only, proving that the module reorganisation hasn't silently broken the
//! stable embedder-facing interface.

use zipp_engine::engine::ZippEngine;
use zipp_engine::object::Object;

/// Smallest possible script: a string literal evaluated at the top level.
#[test]
fn eval_returns_literal() {
    let engine = ZippEngine::default();
    let out = engine.eval(r#""hi""#).expect("eval");
    match out {
        Object::String(s) => assert_eq!(s.as_ref(), "hi"),
        other => panic!("expected String, got {:?}", other),
    }
}

/// Integer arithmetic through the register VM.
#[test]
fn eval_integer_arithmetic() {
    let engine = ZippEngine::default();
    let out = engine.eval("1 + 2 * 3").expect("eval");
    match out {
        Object::Integer(i) => assert_eq!(i, 7),
        other => panic!("expected Integer(7), got {:?}", other),
    }
}

/// Covers the `MulRegConst` fast path bug fix — a register i32 multiplied by
/// an f64 literal must coerce correctly, not reinterpret the f64 bits as an
/// i32 and produce garbage.
#[test]
fn eval_register_int_times_float_constant() {
    let engine = ZippEngine::default();
    let out = engine.eval("let r = 3; 3.14 * r").expect("eval");
    match out {
        Object::Float(f) => assert!((f - 9.42).abs() < 1e-9, "got {}", f),
        other => panic!("expected Float ≈ 9.42, got {:?}", other),
    }
}

/// Covers the register-VM cross-frame throw fix — before the fix, the
/// dispatch loop kept a stale `inst_len` after unwinding and tripped a
/// debug-only bounds check on the very next opcode.
#[test]
fn try_catch_across_function_boundary() {
    let engine = ZippEngine::default();
    let out = engine
        .eval(
            r#"
function thrower() { throw new Error("boom"); }
let result = "FAIL";
try { thrower(); } catch (e) { result = "caught:" + e.message; }
result;
"#,
        )
        .expect("eval");
    match out {
        Object::String(s) => assert_eq!(s.as_ref(), "caught:boom"),
        other => panic!("expected String, got {:?}", other),
    }
}

/// Modern-syntax smoke: classes + inheritance + method dispatch + arrow fns.
#[test]
fn class_inheritance_and_method_dispatch() {
    let engine = ZippEngine::default();
    let out = engine
        .eval(
            r#"
class Shape { constructor(n) { this.name = n; } area() { return 0; } }
class Square extends Shape {
  constructor(s) { super("sq"); this.s = s; }
  area() { return this.s * this.s; }
}
let shapes = [new Square(3), new Square(4)];
shapes.map(s => s.area()).reduce((a, b) => a + b, 0);
"#,
        )
        .expect("eval");
    match out {
        Object::Integer(i) => assert_eq!(i, 25),
        other => panic!("expected Integer(25), got {:?}", other),
    }
}

/// Exercises the LRU bytecode cache by compiling the same script twice.
#[test]
fn repeat_eval_hits_bytecode_cache() {
    let engine = ZippEngine::default();
    let src = "let x = 21; x * 2";
    for _ in 0..5 {
        let out = engine.eval(src).expect("eval");
        match out {
            Object::Integer(i) => assert_eq!(i, 42),
            other => panic!("expected Integer(42), got {:?}", other),
        }
    }
}

/// Verifies the instruction-count execution limit trips for an infinite loop.
#[test]
fn execution_limits_abort_runaway_script() {
    let engine = ZippEngine::default();
    let mut state = engine
        .compile_script("while (true) { }")
        .expect("compile");
    state.set_execution_limits(Some(50_000), Some(1000));
    let err = state.run_init().expect_err("should abort");
    assert!(
        matches!(err, zipp_engine::ZippError::ExecutionLimit(_)),
        "unexpected error: {:?}",
        err
    );
}
