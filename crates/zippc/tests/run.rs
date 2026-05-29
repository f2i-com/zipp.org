//! End-to-end language tests: source -> result / output / error.

use zippc::{run, Value};

fn result(src: &str) -> i64 {
    match run(src).expect("program should run").result {
        Value::I64(x) => x,
        other => panic!("expected i64 result, got {other:?}"),
    }
}
fn fresult(src: &str) -> f64 {
    match run(src).expect("program should run").result {
        Value::F64(f) => f,
        other => panic!("expected f64 result, got {other:?}"),
    }
}
fn output(src: &str) -> Vec<String> {
    run(src).expect("program should run").output
}

#[test]
fn arithmetic_precedence() {
    assert_eq!(result("fn main(): i64 { return 7 * 6 + 3; }"), 45);
    assert_eq!(result("fn main(): i64 { return 2 + 3 * 4 - 10 / 2; }"), 9);
}

#[test]
fn recursion() {
    let src = "fn fib(n: i64): i64 { if (n < 2) { return n; } return fib(n-1) + fib(n-2); } \
               fn main(): i64 { return fib(15); }";
    assert_eq!(result(src), 610);
}

#[test]
fn while_loop_sum_of_squares() {
    let src = "fn main(): i64 { let t = 0; let i = 1; while (i <= 10) { t = t + i*i; i = i + 1; } return t; }";
    assert_eq!(result(src), 385);
}

#[test]
fn bitwise_and_shift() {
    // 12 & 10 = 8 ; 1 << 4 = 16 ; 8 | 16 = 24
    assert_eq!(result("fn main(): i64 { return (12 & 10) | (1 << 4); }"), 24);
    // 0b1010 ^ 0b0110 = 0b1100 = 12
    assert_eq!(result("fn main(): i64 { return 10 ^ 6; }"), 12);
    // ~0 = -1 ; 255 >> 4 = 15
    assert_eq!(result("fn main(): i64 { return ~0; }"), -1);
    assert_eq!(result("fn main(): i64 { return 255 >> 4; }"), 15);
}

#[test]
fn short_circuit_and_avoids_div_by_zero() {
    // If `&&` were eager, `10 / x` would divide by zero and error.
    let src = "fn main(): i64 { let x = 0; let r = 5; if (x != 0 && 10 / x > 0) { r = 1; } return r; }";
    assert_eq!(result(src), 5);
}

#[test]
fn short_circuit_or_avoids_div_by_zero() {
    let src = "fn main(): i64 { let x = 0; let r = 0; if (x == 0 || 10 / x > 0) { r = 1; } return r; }";
    assert_eq!(result(src), 1);
}

#[test]
fn break_and_continue() {
    // Sum odd i, skipping evens (continue), stopping once i > 7 (break) => 1+3+5+7 = 16.
    let src = "fn main(): i64 { let s = 0; let i = 0; \
               while (i < 100) { i = i + 1; if (i % 2 == 0) { continue; } if (i > 7) { break; } s = s + i; } \
               return s; }";
    assert_eq!(result(src), 16);
}

#[test]
fn block_scoping_does_not_leak() {
    // Inner `x` shadows; after the block the outer `x` is unchanged.
    assert_eq!(result("fn main(): i64 { let x = 1; if (true) { let x = 2; } return x; }"), 1);
    // Inner shadow is the one used inside the block.
    assert_eq!(
        result("fn main(): i64 { let x = 1; let y = 0; if (true) { let x = 10; y = x; } return y; }"),
        10
    );
}

#[test]
fn print_collects_output() {
    assert_eq!(
        output("fn main(): i64 { print(1); print(2); print(3); return 0; }"),
        ["1", "2", "3"].map(String::from).to_vec()
    );
}

// ── these must be rejected ──

#[test]
fn rejects_break_outside_loop() {
    assert!(run("fn main(): i64 { break; return 0; }").is_err());
}

#[test]
fn rejects_type_mismatch() {
    assert!(run("fn main(): i64 { return 1 + true; }").is_err());
    assert!(run("fn main(): i64 { if (1) { return 0; } return 0; }").is_err()); // cond not bool
}

#[test]
fn rejects_undeclared_and_out_of_scope() {
    assert!(run("fn main(): i64 { return x; }").is_err());
    // `y` declared only inside the block — not visible after it.
    assert!(run("fn main(): i64 { if (true) { let y = 1; } return y; }").is_err());
}

#[test]
fn rejects_arity_and_unknown_fn() {
    assert!(run("fn f(a: i64): i64 { return a; } fn main(): i64 { return f(1, 2); }").is_err());
    assert!(run("fn main(): i64 { return g(1); }").is_err());
}

// ── f64 ──

#[test]
fn float_arithmetic() {
    assert_eq!(fresult("fn main(): f64 { return 1.5 + 2.5 * 2.0; }"), 6.5);
    assert_eq!(fresult("fn main(): f64 { return 10.0 / 4.0; }"), 2.5);
}

#[test]
fn casts_bridge_int_and_float() {
    // i64 -> f64 then float divide.
    assert_eq!(fresult("fn main(): f64 { let n = 7; return f64(n) / 2.0; }"), 3.5);
    // f64 -> i64 truncates.
    assert_eq!(result("fn main(): i64 { return i64(3.9); }"), 3);
}

#[test]
fn float_comparison_is_bool() {
    assert_eq!(result("fn main(): i64 { if (2.5 > 2.0) { return 1; } return 0; }"), 1);
}

#[test]
fn rejects_mixing_int_and_float() {
    assert!(run("fn main(): f64 { return 1 + 2.0; }").is_err());
    assert!(run("fn main(): i64 { return 5.0 % 2.0; }").is_err()); // % is integer-only
    assert!(run("fn main(): f64 { return 1.0 & 2.0; }").is_err()); // bitwise integer-only
}

#[test]
fn prove_profile_is_integer_only() {
    // A float program runs fine...
    let prog = zippc::compile("fn main(): f64 { return 1.5 + 2.5; }").unwrap();
    assert!(zippc::vm::run(&prog, false).is_ok());
    // ...but recording a trace for it (what --prove does) is rejected.
    assert!(zippc::vm::run(&prog, true).is_err());
}

// ── arrays ──

#[test]
fn array_literal_index_len() {
    assert_eq!(result("fn main(): i64 { let a = [10, 20, 30]; return a[0] + a[2] + len(a); }"), 43);
}

#[test]
fn array_repeat_fill_and_sum() {
    let src = "fn main(): i64 { let a = [0; 5]; let i = 0; \
               while (i < 5) { a[i] = i * i; i = i + 1; } \
               let s = 0; let j = 0; while (j < len(a)) { s = s + a[j]; j = j + 1; } return s; }";
    assert_eq!(result(src), 30); // 0+1+4+9+16
}

#[test]
fn arrays_are_reference_types() {
    // Mutating an array argument is visible to the caller.
    let src = "fn fill(a: [i64]): i64 { a[0] = 99; return 0; } \
               fn main(): i64 { let a = [1, 2, 3]; let _ = fill(a); return a[0]; }";
    assert_eq!(result(src), 99);
}

#[test]
fn float_arrays() {
    assert_eq!(fresult("fn main(): f64 { let a = [1.5, 2.5, 3.0]; return a[0] + a[1] + a[2]; }"), 7.0);
}

#[test]
fn rejects_bad_array_use() {
    assert!(run("fn main(): i64 { let a = [1, 2]; return a[5]; }").is_err()); // out of bounds (runtime)
    assert!(run("fn main(): i64 { let x = 5; return x[0]; }").is_err()); // index a non-array
    assert!(run("fn main(): i64 { let a = [1]; return a[true]; }").is_err()); // non-i64 index
    assert!(run("fn main(): i64 { let a = [1, 2.0]; return a[0]; }").is_err()); // mixed element types
}

#[test]
fn prove_profile_rejects_arrays() {
    let prog = zippc::compile("fn main(): i64 { let a = [1, 2, 3]; return a[1]; }").unwrap();
    assert!(zippc::vm::run(&prog, false).is_ok());
    assert!(zippc::vm::run(&prog, true).is_err());
}

// ── strings ──

#[test]
fn string_concat_len_print() {
    assert_eq!(result("fn main(): i64 { let s = \"foo\" + \"bar\"; return len(s); }"), 6);
    assert_eq!(
        output("fn main(): i64 { print(\"hello, world\"); return 0; }"),
        ["hello, world"].map(String::from).to_vec()
    );
    // string-typed params and returns
    let greet = "fn greet(name: str): str { return \"hi \" + name; } \
                 fn main(): i64 { print(greet(\"zipp\")); return 0; }";
    assert_eq!(output(greet), ["hi zipp"].map(String::from).to_vec());
}

#[test]
fn string_equality_and_escapes() {
    assert_eq!(result("fn main(): i64 { if (\"a\" + \"b\" == \"ab\") { return 1; } return 0; }"), 1);
    assert_eq!(result("fn main(): i64 { if (\"a\" != \"b\") { return 1; } return 0; }"), 1);
    // "a\nb" -> a, newline, b -> 3 bytes
    assert_eq!(result("fn main(): i64 { return len(\"a\\nb\"); }"), 3);
}

#[test]
fn rejects_bad_string_use() {
    assert!(run("fn main(): str { return \"x\" - \"y\"; }").is_err()); // '-' invalid on str
    assert!(run("fn main(): str { return \"x\" + 1; }").is_err()); // mixed str + int
    assert!(run("fn main(): i64 { return \"x\" * 2; }").is_err());
}

#[test]
fn prove_profile_rejects_strings() {
    let prog = zippc::compile("fn main(): i64 { let s = \"hi\"; return len(s); }").unwrap();
    assert!(zippc::vm::run(&prog, false).is_ok());
    assert!(zippc::vm::run(&prog, true).is_err());
}
