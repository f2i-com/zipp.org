//! End-to-end language tests: source -> result / output / error.

use zippc::run;

fn result(src: &str) -> i64 {
    run(src).expect("program should run").result
}
fn output(src: &str) -> Vec<i64> {
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
    assert_eq!(output("fn main(): i64 { print(1); print(2); print(3); return 0; }"), vec![1, 2, 3]);
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
