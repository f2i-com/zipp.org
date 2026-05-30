//! # zipp-vm — dynamic JavaScript engine v2
//!
//! A clean-sheet engine built around one architectural bet: an **explicit-frame
//! register VM**. JS recursion lives in a `Vec` of frames over a flat register
//! file, never the native Rust stack. That gives two things the previous engine
//! lacked by construction:
//!
//! 1. **Bounded, catchable recursion** — deep recursion throws a `RangeError`
//!    instead of overflowing the native stack (a real correctness gap before).
//! 2. **A JIT-ready substrate** — registers are explicit and a value can stay
//!    in one place across a basic block (and, in a later JIT tier, across a
//!    call), which is exactly the property V8 exploits and the old engine could
//!    not preserve through recursion.
//!
//! Pipeline: `oxc` parse → [`compile`] (AST → register bytecode) → [`vm`]
//! (explicit-frame interpreter). This is the interpreter milestone: correct and
//! clean, but NOT yet faster than the old JIT'd engine or V8 — a from-scratch
//! engine starts at zero and earns speed back tier by tier. Numbers are
//! reported honestly at each step.

mod bytecode;
mod compile;
mod heap;
pub mod value;
mod vm;

use oxc_allocator::Allocator;
use oxc_parser::Parser;
use oxc_span::SourceType;

pub use value::Value;

/// Result of running a program: console output plus an optional uncaught-throw
/// message (output produced before the throw is preserved, like a real engine
/// flushing stdout before reporting the error).
pub struct Outcome {
    pub output: Vec<String>,
    pub error: Option<String>,
}

/// Parse + run JavaScript source. `Err` is a compile-time failure (parse error
/// or unsupported syntax); a runtime uncaught throw is reported via
/// [`Outcome::error`] alongside any output produced before it.
pub fn run(src: &str) -> Result<Outcome, String> {
    let allocator = Allocator::default();
    let ret = Parser::new(&allocator, src, SourceType::default()).parse();
    if !ret.errors.is_empty() {
        return Err(format!("SyntaxError: {}", ret.errors[0]));
    }
    let program = compile::compile_program(&ret.program)?;
    let mut vm = vm::Vm::new(&program);
    match vm.run() {
        Ok(_) => Ok(Outcome { output: vm.output, error: None }),
        Err(thrown) => Ok(Outcome {
            output: std::mem::take(&mut vm.output),
            error: Some(thrown.0),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_ok(src: &str) -> Vec<String> {
        let out = run(src).expect("compile");
        assert!(out.error.is_none(), "unexpected throw: {:?}", out.error);
        out.output
    }

    #[test]
    fn console_log_basics() {
        assert_eq!(run_ok("console.log(1, 2, 3)"), vec!["1 2 3"]);
        assert_eq!(run_ok("console.log('hi', true, null)"), vec!["hi true null"]);
    }

    #[test]
    fn arithmetic() {
        assert_eq!(run_ok("console.log(1 + 2 * 3)"), vec!["7"]);
        assert_eq!(run_ok("console.log(10 - 3 - 2)"), vec!["5"]);
        assert_eq!(run_ok("console.log(7 % 3)"), vec!["1"]);
        assert_eq!(run_ok("console.log(7 / 2)"), vec!["3.5"]);
    }

    #[test]
    fn let_and_reassign() {
        assert_eq!(run_ok("let x = 5; x = x + 1; console.log(x)"), vec!["6"]);
        assert_eq!(run_ok("let x = 1; x += 10; console.log(x)"), vec!["11"]);
    }

    #[test]
    fn if_else() {
        assert_eq!(
            run_ok("let x = 3; if (x > 2) { console.log('big') } else { console.log('small') }"),
            vec!["big"]
        );
    }

    #[test]
    fn while_loop() {
        assert_eq!(
            run_ok("let i = 0; let s = 0; while (i < 5) { s = s + i; i = i + 1 } console.log(s)"),
            vec!["10"]
        );
    }

    #[test]
    fn for_loop() {
        assert_eq!(
            run_ok("let s = 0; for (let i = 0; i < 5; i++) { s += i } console.log(s)"),
            vec!["10"]
        );
    }

    #[test]
    fn function_call() {
        assert_eq!(
            run_ok("function add(a, b) { return a + b } console.log(add(3, 4))"),
            vec!["7"]
        );
    }

    #[test]
    fn recursion_fib() {
        assert_eq!(
            run_ok("function fib(n){ return n < 2 ? n : fib(n-1) + fib(n-2) } console.log(fib(10))"),
            vec!["55"]
        );
    }

    #[test]
    fn recursion_is_bounded_not_segfault() {
        // Deeply recursive with no base case reached in bounds → catchable
        // RangeError, NOT a crash.
        let out = run("function r(n){ return r(n+1) } r(0)").expect("compile");
        assert!(out.error.is_some());
        assert!(
            out.error.as_ref().unwrap().contains("Maximum call stack"),
            "expected RangeError, got {:?}",
            out.error
        );
    }

    #[test]
    fn ternary_and_logical() {
        assert_eq!(run_ok("console.log(1 < 2 ? 'a' : 'b')"), vec!["a"]);
        assert_eq!(run_ok("console.log(0 || 'fallback')"), vec!["fallback"]);
        assert_eq!(run_ok("console.log(1 && 2)"), vec!["2"]);
    }

    #[test]
    fn string_concat() {
        assert_eq!(run_ok("console.log('a' + 'b' + 'c')"), vec!["abc"]);
        assert_eq!(run_ok("console.log('n=' + 42)"), vec!["n=42"]);
    }

    // ── Stage 1: reference types ──

    #[test]
    fn array_literal_and_index() {
        assert_eq!(run_ok("let a = [10, 20, 30]; console.log(a[0], a[1], a[2])"), vec!["10 20 30"]);
        assert_eq!(run_ok("let a = [1,2,3]; console.log(a.length)"), vec!["3"]);
        assert_eq!(run_ok("let a = [1,2,3]; a[1] = 99; console.log(a[1])"), vec!["99"]);
    }

    #[test]
    fn array_inspect_matches_node() {
        // node renders arrays with spaced brackets.
        assert_eq!(run_ok("console.log([1, 2, 3])"), vec!["[ 1, 2, 3 ]"]);
        assert_eq!(run_ok("console.log([])"), vec!["[]"]);
    }

    #[test]
    fn array_coercion_is_comma_join() {
        assert_eq!(run_ok("console.log('x' + [1,2,3])"), vec!["x1,2,3"]);
        assert_eq!(run_ok("console.log([1,2,3].join('-'))"), vec!["1-2-3"]);
    }

    #[test]
    fn array_push_pop() {
        assert_eq!(
            run_ok("let a = [1]; a.push(2); a.push(3); console.log(a.length, a[2])"),
            vec!["3 3"]
        );
        assert_eq!(run_ok("let a = [1,2,3]; let x = a.pop(); console.log(x, a.length)"), vec!["3 2"]);
    }

    #[test]
    fn object_literal_and_props() {
        assert_eq!(run_ok("let o = {a: 1, b: 2}; console.log(o.a, o.b)"), vec!["1 2"]);
        assert_eq!(run_ok("let o = {}; o.x = 5; console.log(o.x)"), vec!["5"]);
        assert_eq!(run_ok("let o = {a: 1}; o['b'] = 2; console.log(o['a'], o['b'])"), vec!["1 2"]);
    }

    #[test]
    fn object_inspect_matches_node() {
        assert_eq!(run_ok("console.log({a: 1, b: 2})"), vec!["{ a: 1, b: 2 }"]);
        assert_eq!(run_ok("console.log({})"), vec!["{}"]);
    }

    #[test]
    fn object_reference_semantics() {
        // Aliasing: mutating through one binding is visible through the other.
        assert_eq!(run_ok("let a = {n: 1}; let b = a; b.n = 9; console.log(a.n)"), vec!["9"]);
    }

    #[test]
    fn method_call_with_this() {
        assert_eq!(
            run_ok("let o = {x: 10, get() { return this.x }}; console.log(o.get())"),
            vec!["10"]
        );
    }

    #[test]
    fn this_recursive_method() {
        assert_eq!(
            run_ok("let o = {fact(n){ return n <= 1 ? 1 : n * this.fact(n-1) }}; console.log(o.fact(5))"),
            vec!["120"]
        );
    }

    #[test]
    fn function_expression_and_arrow() {
        assert_eq!(run_ok("let f = function(a){ return a*2 }; console.log(f(21))"), vec!["42"]);
        assert_eq!(run_ok("let g = a => a + 1; console.log(g(41))"), vec!["42"]);
        assert_eq!(run_ok("let h = (a, b) => a * b; console.log(h(6, 7))"), vec!["42"]);
    }

    #[test]
    fn nested_arrays_and_objects_inspect() {
        assert_eq!(run_ok("console.log([1, [2, 3], 4])"), vec!["[ 1, [ 2, 3 ], 4 ]"]);
        assert_eq!(run_ok("console.log({a: [1, 2], b: {c: 3}})"), vec!["{ a: [ 1, 2 ], b: { c: 3 } }"]);
    }

    #[test]
    fn array_as_loop_accumulator() {
        assert_eq!(
            run_ok("let a = []; for (let i = 0; i < 4; i++) { a.push(i * i) } console.log(a.join(','))"),
            vec!["0,1,4,9"]
        );
    }

    // ── Stage 2: callback builtins + string methods ──

    #[test]
    fn array_map_filter_reduce() {
        assert_eq!(
            run_ok("console.log([1,2,3,4].map(x => x * 2).join(','))"),
            vec!["2,4,6,8"]
        );
        assert_eq!(
            run_ok("console.log([1,2,3,4,5,6].filter(x => x % 2 === 0).join(','))"),
            vec!["2,4,6"]
        );
        assert_eq!(
            run_ok("console.log([1,2,3,4].reduce((p, c) => p + c, 0))"),
            vec!["10"]
        );
    }

    #[test]
    fn array_pipeline_matches_corpus() {
        // The exact shape of bench/array.js.
        assert_eq!(
            run_ok("let a=[]; for(let i=0;i<10;i++) a.push(i); console.log(a.map(x=>x*2).filter(x=>x%3===0).reduce((p,c)=>p+c,0))"),
            vec!["36"] // map→0,2,4,…,18; filter %3===0→0,6,12,18; sum→36
        );
    }

    #[test]
    fn array_sort_comparator() {
        assert_eq!(
            run_ok("let a = [3, 1, 4, 1, 5, 9, 2, 6]; a.sort((x, y) => x - y); console.log(a.join(','))"),
            vec!["1,1,2,3,4,5,6,9"]
        );
        // sort returns the same array reference and mutates in place.
        assert_eq!(
            run_ok("let a = [3,1,2]; let b = a.sort((x,y)=>x-y); console.log(a.join(','), a === b)"),
            vec!["1,2,3 true"]
        );
    }

    #[test]
    fn array_sort_default_lexicographic() {
        assert_eq!(
            run_ok("console.log([10, 1, 2, 20].sort().join(','))"),
            vec!["1,10,2,20"]
        );
    }

    #[test]
    fn array_misc_methods() {
        assert_eq!(run_ok("console.log([1,2,3].indexOf(2))"), vec!["1"]);
        assert_eq!(run_ok("console.log([1,2,3].includes(5))"), vec!["false"]);
        assert_eq!(run_ok("console.log([1,2,3,4].slice(1,3).join(','))"), vec!["2,3"]);
        assert_eq!(run_ok("let a=[1,2,3]; console.log(a.shift(), a.join(','))"), vec!["1 2,3"]);
    }

    #[test]
    fn string_indexing_and_methods() {
        assert_eq!(run_ok("let s = 'hello'; console.log(s[0], s[4], s.length)"), vec!["h o 5"]);
        assert_eq!(run_ok("console.log('hello'.toUpperCase())"), vec!["HELLO"]);
        assert_eq!(run_ok("console.log('Hello World'.indexOf('World'))"), vec!["6"]);
        assert_eq!(run_ok("console.log('a,b,c'.split(',').join('-'))"), vec!["a-b-c"]);
        assert_eq!(run_ok("console.log('ab'.repeat(3))"), vec!["ababab"]);
        assert_eq!(run_ok("console.log('hello'.slice(1, 4))"), vec!["ell"]);
    }

    #[test]
    fn string_char_counting_matches_corpus() {
        // The shape of bench/string.js's counting loop.
        assert_eq!(
            run_ok("let s='0123456789'; let c=0; for(let i=0;i<s.length;i++){ if(s[i]==='7') c++; } console.log(c)"),
            vec!["1"]
        );
    }
}
