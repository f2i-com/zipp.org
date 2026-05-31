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
mod capture;
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
mod codegen;
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
    /// Lines from `console.error`/`console.warn` (the caller writes these to
    /// stderr, matching node).
    pub errput: Vec<String>,
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
    // Dev aid: `ZIPP_VM_DUMP=1` prints each function's bytecode to stderr before
    // running (so the JIT-able regions can be inspected).
    if std::env::var_os("ZIPP_VM_DUMP").is_some() {
        for (fid, f) in program.functions.iter().enumerate() {
            eprintln!("── fn {fid} (regs={}, params={}) ──", f.reg_count, f.param_count);
            for (ip, instr) in f.code.iter().enumerate() {
                eprintln!("  {ip:4}  {instr:?}");
            }
        }
    }
    let mut vm = vm::Vm::new(&program);
    match vm.run() {
        Ok(_) => Ok(Outcome { output: vm.output, errput: vm.errput, error: None }),
        Err(thrown) => Ok(Outcome {
            output: std::mem::take(&mut vm.output),
            errput: std::mem::take(&mut vm.errput),
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

    /// Run a program with the JIT forced OFF (pure interpreter), for differential
    /// checks against the default JIT-on `run`.
    fn run_nojit(src: &str) -> Vec<String> {
        let allocator = Allocator::default();
        let ret = Parser::new(&allocator, src, SourceType::default()).parse();
        assert!(ret.errors.is_empty(), "parse error: {:?}", ret.errors);
        let program = compile::compile_program(&ret.program).expect("compile");
        let mut vm = vm::Vm::new(&program);
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        vm.set_jit_enabled(false);
        vm.run().expect("run");
        vm.output
    }

    /// Assert JIT-on output == JIT-off output == `expected` for a hot loop. The
    /// loops here run well past OSR_THRESHOLD so the region JIT (int64 path) fires.
    fn assert_jit_matches(src: &str, expected: &[&str]) {
        let on = run_ok(src);
        let off = run_nojit(src);
        let exp: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
        assert_eq!(on, off, "JIT-on != JIT-off for: {src}");
        assert_eq!(on, exp, "wrong result for: {src}");
    }

    #[test]
    fn int_region_positive_sum() {
        // sum 0..999 = 499500 (stays well within i32).
        assert_jit_matches("let s=0; for(let i=0;i<1000;i++){ s+=i; } console.log(s)", &["499500"]);
    }

    #[test]
    fn int_region_subtraction_negative() {
        // 0 - (0+1+...+999) = -499500 (negative i64, signed flush to Int).
        assert_jit_matches("let s=0; for(let i=0;i<1000;i++){ s-=i; } console.log(s)", &["-499500"]);
    }

    #[test]
    fn int_region_crosses_i32() {
        // sum 0..99999 = 4999950000 > 2^31 — value stays i64 in the loop, flushes
        // as a DOUBLE (since >i32) and must render identically to the interpreter.
        assert_jit_matches("let s=0; for(let i=0;i<100000;i++){ s+=i; } console.log(s)", &["4999950000"]);
    }

    #[test]
    fn int_region_countdown_and_compare() {
        // Decrement with an interior comparison/conditional (exercises bool homes).
        assert_jit_matches(
            "let i=100000; let c=0; while(i>0){ i=i-1; if(i<50000){ c=c+1; } } console.log(c)",
            &["50000"],
        );
    }

    #[test]
    fn int_region_overflow_bail_powers_of_two() {
        // Doubling: s reaches 2^53 then the per-op 2^53 guard bails to the
        // interpreter; results stay exact (powers of two are representable in f64),
        // so JIT-on must equal JIT-off. 2^60's shortest-round-trip form (== node).
        assert_jit_matches(
            "let s=1; for(let i=0;i<60;i++){ s=s+s; } console.log(s)",
            &["1152921504606847000"],
        );
    }

    #[test]
    fn int_region_negative_start_and_loop_var() {
        // Negative live-in and accumulation across zero.
        assert_jit_matches(
            "let s=-1000; for(let i=0;i<2000;i++){ s=s+1; } console.log(s)",
            &["1000"],
        );
    }

    #[test]
    fn int_region_strict_eq_ne() {
        // === and !== as integer comparisons producing bool homes.
        assert_jit_matches(
            "let c=0; for(let i=0;i<1000;i++){ if(i===500){c=c+7;} } console.log(c)",
            &["7"],
        );
        assert_jit_matches(
            "let c=0; for(let i=0;i<1000;i++){ if(i!==0){c=c+1;} } console.log(c)",
            &["999"],
        );
    }

    #[test]
    fn int_region_multi_var_and_bounds() {
        // Several live integer vars + a mix of < and > guards; result spans i32.
        assert_jit_matches(
            "let a=0; let b=1000000; for(let i=0;i<500000;i++){ a=a+2; b=b-1; } console.log(a, b)",
            &["1000000 500000"],
        );
    }

    #[test]
    fn int_region_overflow_nonrepresentable_fibonacci() {
        // Fibonacci grows past 2^53 into integers NOT exactly representable in f64
        // (unlike powers of two), so the int path MUST bail at 2^53 and let the
        // interpreter continue in rounded f64 — JIT-on must equal JIT-off, and
        // both must equal node's value (4660046610375530000 = fib(91) as f64,
        // shortest-round-trip). This is the case the verifier flagged: a value is
        // written, overflows, and must still be flushed correctly.
        assert_jit_matches(
            "let a=0; let b=1; let t=0; for(let i=0;i<90;i++){ t=a+b; a=b; b=t; } console.log(b)",
            &["4660046610375530000"],
        );
    }

    #[test]
    fn heap_region_object_prop_get_set() {
        // GetProp/SetProp in a hot loop (the object.js shape): o.a=i; o.b=o.a+1;
        // s+=o.b. sum of (i+1) for i in 0..999 = sum 1..1000 = 500500.
        assert_jit_matches(
            "let o={a:0,b:0}; let s=0; for(let i=0;i<1000;i++){ o.a=i; o.b=o.a+1; s+=o.b; } console.log(s)",
            &["500500"],
        );
    }

    #[test]
    fn heap_region_object_read_only_and_mul() {
        // Read a stable property each iteration + Mul (forces the double/mem path,
        // not int64). o.k*3 summed: 3*7*2000 = 42000.
        assert_jit_matches(
            "let o={k:7}; let s=0; for(let i=0;i<2000;i++){ s += o.k*3; } console.log(s)",
            &["42000"],
        );
    }

    #[test]
    fn int_region_multiply() {
        // i*i in the int64 path (imul). sum_{i<10000} i^2 = (n-1)n(2n-1)/6, n=10000.
        assert_jit_matches(
            "let s=0; for(let i=0;i<10000;i++){ s += i*i; } console.log(s)",
            &["333283335000"],
        );
    }

    #[test]
    fn int_region_multiply_overflow_bails() {
        // Repeated doubling via multiply crosses 2^53 → the per-op guard bails to
        // the interpreter; powers of two stay exact, so JIT-on == JIT-off == node.
        // 2^60 = 1152921504606847000 (shortest round-trip).
        assert_jit_matches(
            "let p=1; for(let i=0;i<60;i++){ p=p*2; } console.log(p)",
            &["1152921504606847000"],
        );
    }

    #[test]
    fn object_sroa_full_chain_int_mul() {
        // The object.js chain — exercises object scalar-replacement + int64 Mul
        // (o.c = o.b*2). s = sum 2*(i+1) for i in 0..999 = 1001000. (Also covered by
        // heap_region_object_full_chain, but at the scale that triggers SROA.)
        assert_jit_matches(
            "let o={a:0,b:0,c:0}; let s=0; for(let i=0;i<5000;i++){ o.a=i; o.b=o.a+1; o.c=o.b*2; s+=o.c; } console.log(s)",
            &["25005000"],
        );
    }

    #[test]
    fn regalloc_linear_scan_reuse_many_values() {
        // A loop with far more numeric values (~33) than the 14-home pool, forcing
        // linear-scan home REUSE. Hoisted constants (1..16) must keep permanent
        // homes (a reused home would clobber them — a real bug this guards).
        // s = sum_{i<100000} sum_{k=1..16}(i+k) = 16*sum(i) + 136*100000.
        assert_jit_matches(
            "let s=0; for(let i=0;i<100000;i++){ s += (i+1)+(i+2)+(i+3)+(i+4)+(i+5)+(i+6)+(i+7)+(i+8)+(i+9)+(i+10)+(i+11)+(i+12)+(i+13)+(i+14)+(i+15)+(i+16); } console.log(s)",
            &["80012800000"],
        );
    }

    #[test]
    fn heap_region_setprop_on_array_noops() {
        // Setting a property on an ARRAY is a silent no-op in this engine (only
        // plain Objects store props) — the JIT must match the interpreter (return
        // success, NOT deopt-churn). The loop stays JIT'd; s = sum 0..999 = 499500.
        assert_jit_matches(
            "let a=[]; let s=0; for(let i=0;i<1000;i++){ a.x=i; s+=i; } console.log(s)",
            &["499500"],
        );
    }

    #[test]
    fn heap_region_object_full_chain() {
        // The exact object.js chain at smaller scale: o.a=i; o.b=o.a+1; o.c=o.b*2;
        // s+=o.c. s = sum 2*(i+1) for i in 0..999 = 2*(1+..+1000) = 1001000.
        assert_jit_matches(
            "let o={a:0,b:0,c:0}; let s=0; for(let i=0;i<1000;i++){ o.a=i; o.b=o.a+1; o.c=o.b*2; s+=o.c; } console.log(s)",
            &["1001000"],
        );
    }

    #[test]
    fn large_whole_double_uses_shortest_roundtrip() {
        // JS Number→String prints the shortest decimal that round-trips, and a
        // whole double above i64::MAX must not overflow `as i64`.
        assert_eq!(run_ok("console.log(4660046610375530496)"), vec!["4660046610375530000"]);
        assert_eq!(run_ok("console.log(1e20)"), vec!["100000000000000000000"]);
        assert_eq!(run_ok("console.log(1e19)"), vec!["10000000000000000000"]);
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

    // ── Stage 3: closures ──

    #[test]
    fn closure_counter_shares_mutable_state() {
        // The classic: each call mutates the captured `c`, shared across calls.
        assert_eq!(
            run_ok("function counter(){ let c=0; return function(){ c++; return c } } let f=counter(); console.log(f(), f(), f())"),
            vec!["1 2 3"]
        );
    }

    #[test]
    fn closure_captures_parameter() {
        assert_eq!(
            run_ok("function adder(n){ return x => x + n } let a5 = adder(5); console.log(a5(10), a5(20))"),
            vec!["15 25"]
        );
    }

    #[test]
    fn arrow_captures_outer_let() {
        assert_eq!(run_ok("let mul = 3; let f = x => x * mul; console.log(f(10))"), vec!["30"]);
    }

    #[test]
    fn nested_of_nested_capture() {
        // Three levels: innermost captures `a` from the grandparent (ParentUpval
        // re-sourcing) and `b` from the parent (ParentLocal).
        assert_eq!(
            run_ok("function outer(){ let a=1; function mid(){ let b=10; return function(){ return a+b } } return mid() } console.log(outer()())"),
            vec!["11"]
        );
    }

    #[test]
    fn closures_are_independent_instances() {
        // Two counters from the same factory must not share state.
        assert_eq!(
            run_ok("function mk(){ let c=0; return ()=>++c } let a=mk(); let b=mk(); console.log(a(),a(),b(),a())"),
            vec!["1 2 1 3"]
        );
    }

    #[test]
    fn closure_mutates_captured_from_inner() {
        // Writing a captured upvalue from the inner function is visible to a
        // sibling reader closure (shared cell).
        assert_eq!(
            run_ok("function mk(){ let v=0; let set=x=>{v=x}; let get=()=>v; return [set,get] } let p=mk(); p[0](42); console.log(p[1]())"),
            vec!["42"]
        );
    }

    // ── Stage 4: loose equality ──

    #[test]
    fn loose_equality_matches_node() {
        assert_eq!(run_ok("console.log(1 == '1')"), vec!["true"]);
        assert_eq!(run_ok("console.log(null == undefined)"), vec!["true"]);
        assert_eq!(run_ok("console.log(0 == false)"), vec!["true"]);
        assert_eq!(run_ok("console.log('' == 0)"), vec!["true"]);
        assert_eq!(run_ok("console.log('2' == 2)"), vec!["true"]);
        assert_eq!(run_ok("console.log(true == 1)"), vec!["true"]);
        assert_eq!(run_ok("console.log(null == 0)"), vec!["false"]);
        assert_eq!(run_ok("console.log(undefined == 0)"), vec!["false"]);
        assert_eq!(run_ok("console.log(1 != '1')"), vec!["false"]);
        assert_eq!(run_ok("console.log(null != undefined)"), vec!["false"]);
    }

    #[test]
    fn strict_vs_loose_distinct() {
        assert_eq!(run_ok("console.log(1 === '1', 1 == '1')"), vec!["false true"]);
        assert_eq!(run_ok("console.log(null === undefined, null == undefined)"), vec!["false true"]);
    }

    #[test]
    fn nan_and_infinity_globals() {
        assert_eq!(run_ok("console.log(NaN == NaN)"), vec!["false"]);
        assert_eq!(run_ok("let x = 0/0; console.log(x === x)"), vec!["false"]);
        assert_eq!(run_ok("console.log(Infinity > 1e308, -Infinity < 0)"), vec!["true true"]);
    }

    // ── Stage 4: for-of / for-in / do-while / try-catch-throw ──

    #[test]
    fn for_of_array_and_string() {
        assert_eq!(run_ok("let s=0; for (const x of [1,2,3,4]) { s += x } console.log(s)"), vec!["10"]);
        assert_eq!(run_ok("let s=''; for (const c of 'abc') { s = c + s } console.log(s)"), vec!["cba"]);
    }

    #[test]
    fn for_in_object_keys_and_values() {
        assert_eq!(run_ok("let o={a:1,b:2,c:3}; let k=''; for (const key in o) { k += key } console.log(k)"), vec!["abc"]);
        assert_eq!(run_ok("let o={x:10,y:20,z:5}; let s=0; for (const key in o) { s += o[key] } console.log(s)"), vec!["35"]);
    }

    #[test]
    fn do_while_runs_body_first() {
        assert_eq!(run_ok("let i=0,s=0; do { s+=i; i++ } while (i<5); console.log(s)"), vec!["10"]);
        // body runs at least once even when the condition is false initially
        assert_eq!(run_ok("let n=0; do { n++ } while (false); console.log(n)"), vec!["1"]);
    }

    #[test]
    fn try_catch_basic() {
        assert_eq!(run_ok("try { throw 'boom' } catch (e) { console.log('caught', e) }"), vec!["caught boom"]);
        assert_eq!(run_ok("try { throw 42 } catch (e) { console.log(e + 1) }"), vec!["43"]);
    }

    #[test]
    fn try_catch_across_call() {
        assert_eq!(
            run_ok("function f(){ throw 'deep' } try { f() } catch(e){ console.log('got', e) }"),
            vec!["got deep"]
        );
    }

    #[test]
    fn try_catch_finally_order() {
        assert_eq!(
            run_ok("let r=''; try { r+='a'; throw 1; r+='b' } catch(e){ r+='c' } finally { r+='d' } console.log(r)"),
            vec!["acd"]
        );
        // finally also runs on normal completion
        assert_eq!(
            run_ok("let r=''; try { r+='x' } finally { r+='y' } console.log(r)"),
            vec!["xy"]
        );
    }

    #[test]
    fn try_finally_runs_on_all_exits() {
        // `return` inside try runs the finally first (sync function).
        assert_eq!(run_ok("function f(){try{return 'A'}finally{console.log('fin')}} console.log(f())"), vec!["fin", "A"]);
        // Plain `return` in try/finally, no catch.
        assert_eq!(run_ok("function f(){try{return 1}finally{console.log('f')}} console.log(f())"), vec!["f", "1"]);
        // Nested try/finally: both finallys run, innermost first.
        assert_eq!(run_ok("function f(){try{try{return 'v'}finally{console.log('in')}}finally{console.log('out')}} console.log(f())"), vec!["in", "out", "v"]);
        // finally overrides the try's return.
        assert_eq!(run_ok("function f(){try{return 'try'}finally{return 'fin'}} console.log(f())"), vec!["fin"]);
        // finally overrides a throw with a return.
        assert_eq!(run_ok("function f(){try{throw 'x'}finally{return 'saved'}} console.log(f())"), vec!["saved"]);
        // A throw in finally overrides the try's return; caught one level out.
        assert_eq!(run_ok("function f(){try{try{return 'x'}finally{throw 'ft'}}catch(e){return 'caught '+e}} console.log(f())"), vec!["caught ft"]);
        // Throw propagates THROUGH a finally (uncaught locally) across a call.
        assert_eq!(run_ok("function g(){try{throw 'd'}finally{console.log('gfin')}} function f(){try{g()}catch(e){return 'c '+e}} console.log(f())"), vec!["gfin", "c d"]);
        // finally runs every loop iteration; value passes through on normal exit.
        assert_eq!(run_ok("function f(){let s=0; for(let i=0;i<3;i++){try{s+=i}finally{s+=100}} return s} console.log(f())"), vec!["303"]);
        // return in catch, with finally still running.
        assert_eq!(run_ok("function f(){try{throw 'e'}catch(x){return 'c'}finally{console.log('fin')}} console.log(f())"), vec!["fin", "c"]);
    }

    #[test]
    fn error_object_name_and_message() {
        assert_eq!(run_ok("try { throw new Error('boom') } catch (e) { console.log(e.message, e.name) }"), vec!["boom Error"]);
        assert_eq!(run_ok("try { throw new RangeError('neg') } catch (e) { console.log(e.name) }"), vec!["RangeError"]);
    }

    #[test]
    fn property_of_undefined_throws_typeerror() {
        assert_eq!(
            run_ok("let x; try { x = undefined.foo } catch (e) { x = 'caught' } console.log(x)"),
            vec!["caught"]
        );
    }

    #[test]
    fn uncaught_throw_reports_error_with_output_preserved() {
        let out = run("console.log('before'); throw new Error('fail'); console.log('after')").expect("compile");
        assert_eq!(out.output, vec!["before"]);
        assert!(out.error.as_ref().unwrap().contains("fail"), "got {:?}", out.error);
    }

    // ── Stage 5b: native JIT (correctness — same answers as the interpreter) ──

    #[test]
    fn jit_hot_int_leaf_function_correct() {
        // A pure-int leaf function called far past the JIT threshold (8). The
        // result must equal node regardless of whether it ran native or interp.
        assert_eq!(
            run_ok("function sq(x){ return x*x } let s=0; for(let i=0;i<50;i++){ s = s + sq(i) } console.log(s)"),
            vec!["40425"] // sum of i^2 for i in 0..49
        );
    }

    #[test]
    fn jit_multi_op_int_function_correct() {
        // f(a,b,c)=max(0, a*a + 2b - c); summed over i in 0..30 → 455 (node).
        assert_eq!(
            run_ok("function f(a,b,c){ let r=a*a; r=r+b*2; r=r-c; if(r<0) return 0; return r } let t=0; for(let i=0;i<30;i++){ t=t+f(i%7, i%5, i%3) } console.log(t)"),
            vec!["455"]
        );
    }

    #[test]
    fn jit_overflow_bails_to_f64_not_wrap() {
        // i32 multiply that overflows must NOT wrap (the old engine's bug); the
        // JIT bails and the interpreter computes the f64 result, == node.
        assert_eq!(
            run_ok("function big(x){ return x*x } let r=0; for(let i=0;i<20;i++){ r = big(100000) } console.log(r)"),
            vec!["10000000000"] // 100000^2 = 1e10, exceeds i32 → must be exact, not wrapped
        );
    }

    #[test]
    fn jit_type_change_bails_correctly() {
        // A function that's int for many calls then gets a non-int arg must
        // still produce the right answer (the op bails on the non-int operand).
        assert_eq!(
            run_ok("function add1(x){ return x + 1 } let out=''; for(let i=0;i<12;i++){ out = '' + add1(i) } out = '' + add1('s'); console.log(out)"),
            vec!["s1"] // 's' + 1 → 's1' (string concat, via bail)
        );
    }

    #[test]
    fn per_iteration_let_bindings() {
        // A captured `let` loop var gets a FRESH binding per iteration (node 0,1,2),
        // while `var` shares one (3,3,3). Covers for / for-of / for-in.
        assert_eq!(
            run_ok("function mk(){ let xs=[]; for(let i=0;i<3;i++){ xs.push(()=>i) } return xs } let f=mk(); console.log(f.map(g=>g()).join(','))"),
            vec!["0,1,2"]
        );
        assert_eq!(
            run_ok("function mk(){ let xs=[]; for(var j=0;j<3;j++){ xs.push(()=>j) } return xs } console.log(mk().map(g=>g()).join(','))"),
            vec!["3,3,3"]
        );
        // for-of: fresh binding per element.
        assert_eq!(
            run_ok("function mk(){ let xs=[]; for(let x of [10,20,30]){ xs.push(()=>x) } return xs } console.log(mk().map(g=>g()).join(','))"),
            vec!["10,20,30"]
        );
        // for-in: fresh binding per key.
        assert_eq!(
            run_ok("function mk(){ let xs=[]; let o={a:1,b:2}; for(let k in o){ xs.push(()=>k) } return xs } console.log(mk().map(g=>g()).join(','))"),
            vec!["a,b"]
        );
        // Mutation inside the body is visible to THAT iteration's closure.
        assert_eq!(
            run_ok("function mk(){ let xs=[]; for(let i=0;i<3;i++){ i+=10; xs.push(()=>i) } return xs } console.log(mk().map(g=>g()).join(','))"),
            vec!["10"]
        );
        // Nested for-let captures independently.
        assert_eq!(
            run_ok("function mk(){ let xs=[]; for(let a=0;a<2;a++) for(let b=0;b<2;b++) xs.push(()=>a*10+b); return xs } console.log(mk().map(g=>g()).join(','))"),
            vec!["0,1,10,11"]
        );
        // Non-captured loop is unaffected (fast path / hot-loop JIT preserved).
        assert_eq!(run_ok("let s=0; for(let i=0;i<1000;i++) s+=i; console.log(s)"), vec!["499500"]);
    }

    // ── rope strings (cons-strings) + JsStr cached length/index + interning ──

    #[test]
    fn rope_concat_loop_content_and_length() {
        // `s += digit` builds a deep rope; flattening on display + the O(1)
        // cached length must reproduce the eager-concat result exactly.
        assert_jit_matches(
            "let s=''; for(let i=0;i<5;i++){ s += i; } console.log(s, s.length)",
            &["01234 5"],
        );
    }

    #[test]
    fn rope_index_and_methods_after_flatten() {
        // First s[i] flattens the rope; charAt / indexOf / split / toUpperCase
        // must then operate on the flat string correctly.
        assert_eq!(
            run_ok("let s=''; for(let i=0;i<5;i++){ s+=i; } console.log(s.charAt(2), s.indexOf('3'), s.split('').length, s.toUpperCase())"),
            vec!["2 3 5 01234"],
        );
    }

    #[test]
    fn rope_aliasing_is_immutable() {
        // `let t=s; s+=x` must NOT mutate t — ropes share children structurally,
        // and flattening s in place must not corrupt the aliased value t.
        assert_eq!(
            run_ok("let s=''; for(let i=0;i<3;i++){ s+='ab'; } let t=s; s+='Z'; console.log(s, t, s.length, t.length)"),
            vec!["abababZ ababab 7 6"],
        );
    }

    #[test]
    fn rope_strict_eq_against_flat() {
        // A rope and a flat literal with equal content are === equal (str_eq
        // materializes the rope side; flat-vs-flat stays the fast no-alloc path).
        assert_eq!(
            run_ok("let a='he'+'llo'; console.log(a==='hello', a==='hell', ('x'+'y')===('xy'))"),
            vec!["true false true"],
        );
    }

    #[test]
    fn empty_rope_length_and_truthiness() {
        // An empty rope ("" + "") has length 0 and is falsy (str_is_empty is O(1)
        // on Cons via len, and the interned empty string round-trips).
        assert_eq!(
            run_ok("let e=''+''; console.log(e.length, e?1:0, (''+'')==='')"),
            vec!["0 0 true"],
        );
    }

    #[test]
    fn concat_coerces_array_and_object() {
        // Either side heap ⇒ string concatenation; arrays join, objects become
        // [object Object] — coerced to a flat string child of the rope.
        assert_eq!(
            run_ok("console.log([1,2]+[3], {}+'x', 'n='+(1+2))"),
            vec!["1,23 [object Object]x n=3"],
        );
    }

    #[test]
    fn interned_single_chars_index_correctly() {
        // Indexing returns interned single-char strings (shared slots); content
        // and per-index correctness must hold across many accesses.
        assert_jit_matches(
            "let s='abcdefghij'; let c=0; for(let i=0;i<s.length;i++){ if(s[i]==='e'){ c++; } } console.log(c, s[0], s[9])",
            &["1 a j"],
        );
    }

    #[test]
    fn nonascii_length_and_index_scalar_count() {
        // Non-ASCII falls back to chars().nth (scalar indexing); .length is the
        // cached scalar count. 'café' is 4 scalars; index 3 is 'é'.
        assert_eq!(
            run_ok("let s='caf\u{00e9}'; console.log(s.length, s[3], s.charAt(3), (s+s).length)"),
            vec!["4 \u{00e9} \u{00e9} 8"],
        );
    }

    #[test]
    fn array_index_region_sum() {
        // `s += a[i]` over a constant bound JITs in the OSR region (GetIndex via
        // helper). The region computes the loop counter as f64, so a[i] indexes
        // with a DOUBLE key — array_index must coerce it. JIT-on == JIT-off.
        assert_jit_matches(
            "let a=[]; for(let i=0;i<100;i++) a.push(i); let s=0; for(let i=0;i<100;i++){ s+=a[i]; } console.log(s)",
            &["4950"],
        );
    }

    #[test]
    fn array_length_bound_index_region() {
        // `for (i < a.length) s += a[i]` — the common array-scan shape. a.length
        // is a GetProp the miss-helper now answers for arrays (uncached), so the
        // whole loop JITs instead of bailing on the first .length access.
        assert_jit_matches(
            "let a=[]; for(let i=0;i<100;i++) a.push(i*2); let s=0; for(let i=0;i<a.length;i++){ s+=a[i]; } console.log(s)",
            &["9900"],
        );
    }

    #[test]
    fn object_length_property_not_confused_with_array_length() {
        // An object with its own `length` property reads that property via the
        // inline-cache slot, never the array element-count path.
        assert_jit_matches(
            "let o={length:7}; let s=0; for(let i=0;i<20000;i++){ s+=o.length; } console.log(s)",
            &["140000"],
        );
    }

    #[test]
    fn switch_statement() {
        assert_eq!(run_ok("let x=2,r=''; switch(x){case 1:r='a';break;case 2:r='b';break;default:r='d';} console.log(r)"), vec!["b"]);
        assert_eq!(run_ok("let x=9,r=''; switch(x){case 1:r='a';break;default:r='d';} console.log(r)"), vec!["d"]);
        // Fall-through (no break) runs subsequent case bodies.
        assert_eq!(run_ok("let r='',x=2; switch(x){case 1:r+='1';case 2:r+='2';case 3:r+='3';break;case 4:r+='4';} console.log(r)"), vec!["23"]);
        assert_eq!(run_ok("function f(x){switch(x){case 1:return 'one';default:return 'other'}} console.log(f(1),f(5))"), vec!["one other"]);
        // `continue` in a switch targets the enclosing LOOP; `break` targets the switch.
        assert_eq!(run_ok("let r=[]; for(let i=0;i<4;i++){ switch(i){case 1:continue;case 3:break;} r.push(i); } console.log(r.join(','))"), vec!["0,2,3"]);
    }

    #[test]
    fn break_and_continue() {
        assert_eq!(run_ok("let s=0; for(let i=0;i<10;i++){ if(i===5) break; s+=i; } console.log(s)"), vec!["10"]);
        assert_eq!(run_ok("let s=0; for(let i=0;i<5;i++){ if(i===2) continue; s+=i; } console.log(s)"), vec!["8"]);
        assert_eq!(run_ok("let s=0,i=0; while(i<100){ i++; if(i>5) break; s+=i; } console.log(s)"), vec!["15"]);
        assert_eq!(run_ok("let s=0; for(const x of [1,2,3,4]){ if(x===3) break; s+=x; } console.log(s)"), vec!["3"]);
        // do-while with both; and a nested loop where the inner break stays inner.
        assert_eq!(run_ok("let s=0,i=0; do{ i++; if(i===3) continue; if(i>5) break; s+=i; }while(i<100); console.log(s)"), vec!["12"]);
        assert_eq!(run_ok("let s=0; for(let i=0;i<5;i++){ for(let j=0;j<5;j++){ if(j===2) break; s+=1; } } console.log(s)"), vec!["10"]);
    }

    #[test]
    fn break_in_hot_loop_jit() {
        // A `break` in a JIT'd loop is a region exit; JIT-on must equal JIT-off.
        assert_jit_matches(
            "let c=0; for(let i=0;i<100000;i++){ if(i>=50000) break; c++; } console.log(c)",
            &["50000"],
        );
    }

    #[test]
    fn optional_chaining() {
        // Member chains short-circuit to undefined at the first nullish base.
        assert_eq!(run_ok("let o={a:{b:7}}; console.log(o?.a?.b, o?.x?.y, o?.a?.b?.c)"), vec!["7 undefined undefined"]);
        assert_eq!(run_ok("let o=null; console.log(o?.a?.b)"), vec!["undefined"]);
        // Optional computed access and optional calls.
        assert_eq!(run_ok("let o={arr:[10,20]}; console.log(o?.arr?.[1], o?.no?.[0])"), vec!["20 undefined"]);
        assert_eq!(run_ok("let o={f:()=>42}; console.log(o?.f(), o?.g?.())"), vec!["42 undefined"]);
        // The short-circuited value is genuine undefined (NaN in arithmetic).
        assert_eq!(run_ok("let u=undefined; console.log(u?.x, (u?.x)+1)"), vec!["undefined NaN"]);
    }

    #[test]
    fn default_parameters() {
        // Applied only when the arg is missing/undefined (null does NOT trigger it).
        assert_eq!(run_ok("function f(x=5){return x} console.log(f(), f(9), f(undefined))"), vec!["5 9 5"]);
        assert_eq!(run_ok("function z(x=1){return x} console.log(z(null), z(0))"), vec!["null 0"]);
        // A later default may reference an earlier parameter.
        assert_eq!(run_ok("function g(a,b=10,c=a+b){return a+','+b+','+c} console.log(g(1), g(1,2))"), vec!["1,10,11 1,2,3"]);
        // Arrow defaults; and a defaulted parameter captured by a closure.
        assert_eq!(run_ok("let h=(x=7)=>x*2; console.log(h(), h(4))"), vec!["14 8"]);
        assert_eq!(run_ok("function cap(n=3){return ()=>n} console.log(cap()(), cap(8)())"), vec!["3 8"]);
    }

    #[test]
    fn array_is_array() {
        assert_eq!(
            run_ok("console.log(Array.isArray([]), Array.isArray([1]), Array.isArray(1), Array.isArray('x'), Array.isArray({}), Array.isArray(null))"),
            vec!["true true false false false false"],
        );
    }

    #[test]
    fn number_parse_globals() {
        assert_eq!(
            run_ok("console.log(Number('42')+1, Number(''), Number(true), Number('abc'), Number())"),
            vec!["43 0 1 NaN 0"],
        );
        assert_eq!(
            run_ok("console.log(parseInt('10px'), parseInt('0xff'), parseInt('11',2), parseInt('-7'), parseInt('abc'))"),
            vec!["10 255 3 -7 NaN"],
        );
        assert_eq!(
            run_ok("console.log(parseFloat('3.14x'), parseFloat('1e3'), parseFloat('-2.5e-1'), parseFloat('abc'))"),
            vec!["3.14 1000 -0.25 NaN"],
        );
    }

    #[test]
    fn method_name_after_numeric_constant() {
        // REGRESSION: a method/property name's index must be into string_constants,
        // not the constant pool — a preceding non-string constant (e.g. 3.5) used
        // to push the name's pool index past string_constants and panic (OOB).
        assert_eq!(run_ok("console.log((3.5).toFixed(2))"), vec!["3.50"]);
        assert_eq!(run_ok("let x=3.14159; console.log(x.toFixed(2))"), vec!["3.14"]);
        assert_eq!(run_ok("let n=9.5; let o={prop:7}; console.log(o.prop, n)"), vec!["7 9.5"]);
        assert_eq!(run_ok("let a=[1.5]; console.log(a[0].toFixed(1))"), vec!["1.5"]);
    }

    #[test]
    fn object_statics_and_math_constants() {
        assert_eq!(
            run_ok("let o={a:1,b:2,c:3}; console.log(Object.keys(o).join(','), Object.values(o).join(','))"),
            vec!["a,b,c 1,2,3"],
        );
        assert_eq!(
            run_ok("let o={x:10,y:20}; console.log(Object.entries(o).map(e=>e[0]+'='+e[1]).join(','))"),
            vec!["x=10,y=20"],
        );
        assert_eq!(run_ok("console.log(Object.keys([7,8]).join(','), Object.values([7,8]).join(','))"), vec!["0,1 7,8"]);
        assert_eq!(run_ok("console.log(Math.PI.toFixed(4), Math.E.toFixed(4), Math.SQRT2.toFixed(4))"), vec!["3.1416 2.7183 1.4142"]);
    }

    #[test]
    fn json_parse() {
        assert_eq!(
            run_ok("let o=JSON.parse('{\"a\":1,\"b\":[2,3],\"c\":\"hi\"}'); console.log(o.a, o.b[1], o.c)"),
            vec!["1 3 hi"],
        );
        assert_eq!(
            run_ok("console.log(JSON.parse('[1,2.5,-3,1e2,true,false,null]').join(','))"),
            vec!["1,2.5,-3,100,true,false,"],
        );
        // Round-trips with stringify.
        assert_eq!(run_ok("let r=JSON.parse(JSON.stringify({x:[1,{y:2}],z:'a'})); console.log(r.x[1].y, r.z)"), vec!["2 a"]);
        // Invalid JSON throws a (catchable) SyntaxError.
        assert_eq!(run_ok("let e='ok'; try{ JSON.parse('{bad}'); }catch(x){ e='threw'; } console.log(e)"), vec!["threw"]);
        assert_eq!(run_ok("let e='ok'; try{ JSON.parse('[1,2'); }catch(x){ e='threw'; } console.log(e)"), vec!["threw"]);
    }

    #[test]
    fn json_stringify() {
        assert_eq!(run_ok("console.log(JSON.stringify({a:1,b:[2,3]}))"), vec![r#"{"a":1,"b":[2,3]}"#]);
        // undefined/function are omitted in objects but become null in arrays.
        assert_eq!(run_ok("console.log(JSON.stringify([1,undefined,null]))"), vec!["[1,null,null]"]);
        assert_eq!(run_ok("console.log(JSON.stringify({x:undefined,y:1}))"), vec![r#"{"y":1}"#]);
        // Primitives; NaN/Infinity → null; top-level undefined → undefined.
        assert_eq!(run_ok("console.log(JSON.stringify(42), JSON.stringify(NaN), JSON.stringify(undefined))"), vec!["42 null undefined"]);
        // Pretty-print with a numeric `space`.
        assert_eq!(run_ok("console.log(JSON.stringify({a:1}, null, 2))"), vec!["{\n  \"a\": 1\n}"]);
    }

    #[test]
    fn spread_operator() {
        // Array-literal spread: arrays, repeated sources, with plain elements.
        assert_eq!(run_ok("let a=[1,2]; console.log([...a,3,...a].join(','))"), vec!["1,2,3,1,2"]);
        assert_eq!(run_ok("let a=[1,2],b=[3,4]; console.log([0,...a,...b,5].join(','))"), vec!["0,1,2,3,4,5"]);
        assert_eq!(run_ok("console.log([...[]].length, [...[1]].length)"), vec!["0 1"]);
        // Spreading a string yields its characters.
        assert_eq!(run_ok("console.log([...'abc'].join('-'))"), vec!["a-b-c"]);
        // Call spread on a plain function value (declared fn and arrow).
        assert_eq!(run_ok("function sum(a,b,c){return a+b+c} console.log(sum(...[1,2,3]))"), vec!["6"]);
        assert_eq!(run_ok("let g=(a,b)=>a-b; console.log(g(...[10,3]))"), vec!["7"]);
        assert_eq!(run_ok("function f(a,b,c,d){return a+b+c+d} console.log(f(1,...[2,3],4))"), vec!["10"]);
        // Method-call spread: builtin (push/concat) and mixed spread+plain args.
        assert_eq!(run_ok("let a=[3,1,2]; a.push(...[4,5]); console.log(a.join(','))"), vec!["3,1,2,4,5"]);
        assert_eq!(run_ok("let a=[1,2],b=[5,6]; a.push(...b,7); console.log(a.join(','))"), vec!["1,2,5,6,7"]);
        assert_eq!(run_ok("console.log([0].concat(...[[1,2],[3]]).join(','))"), vec!["0,1,2,3"]);
        // Spreading a non-iterable throws a (catchable) TypeError.
        assert_eq!(run_ok("let e='ok'; try{ [...5]; }catch(x){ e='threw'; } console.log(e)"), vec!["threw"]);
    }

    #[test]
    fn destructuring() {
        // Object: shorthand, subset, rename, defaults.
        assert_eq!(run_ok("let {x,y}={x:1,y:2}; console.log(x+y)"), vec!["3"]);
        assert_eq!(run_ok("let {a:p,b:q}={a:10,b:20}; console.log(p,q)"), vec!["10 20"]);
        assert_eq!(run_ok("let {x=5,y=9}={x:1}; console.log(x,y)"), vec!["1 9"]);
        // Array: positional, holes, defaults, rest (incl. shorter-than-pattern).
        assert_eq!(run_ok("let [a,b,c]=[10,20,30]; console.log(a+b+c)"), vec!["60"]);
        assert_eq!(run_ok("let [,b,,d]=[1,2,3,4]; console.log(b,d)"), vec!["2 4"]);
        assert_eq!(run_ok("let [a=1,b=2,c=3]=[10]; console.log(a,b,c)"), vec!["10 2 3"]);
        assert_eq!(run_ok("let [first,...rest]=[1,2,3,4]; console.log(first, rest.join(','))"), vec!["1 2,3,4"]);
        assert_eq!(run_ok("let [a,b,...rest]=[1]; console.log(a,b,rest.length)"), vec!["1 undefined 0"]);
        // A string is iterable for array destructuring.
        assert_eq!(run_ok("let [h,...t]='hello'; console.log(h, t.join(''))"), vec!["h ello"]);
        // Nested patterns, arbitrary depth.
        assert_eq!(run_ok("let {a:{b}}={a:{b:42}}; console.log(b)"), vec!["42"]);
        assert_eq!(run_ok("let [[a,b],[c]]=[[1,2],[3]]; console.log(a,b,c)"), vec!["1 2 3"]);
        assert_eq!(run_ok("let {p:[m,n]}={p:[7,8]}; console.log(m,n)"), vec!["7 8"]);
        // Computed key.
        assert_eq!(run_ok("let k='x'; let {[k]:v}={x:99}; console.log(v)"), vec!["99"]);
        // Object rest: collects the remaining own keys into a new object.
        assert_eq!(run_ok("let {a,...rest}={a:1,b:2,c:3}; console.log(a, JSON.stringify(rest))"), vec![r#"1 {"b":2,"c":3}"#]);
        assert_eq!(run_ok("let {a:x,...rest}={a:1,b:2}; console.log(x, JSON.stringify(rest))"), vec![r#"1 {"b":2}"#]);
        assert_eq!(run_ok("let f=({id,...opts})=>id+':'+JSON.stringify(opts); console.log(f({id:1,a:2,b:3}))"), vec![r#"1:{"a":2,"b":3}"#]);
        // Inside a function; a destructured local captured by a closure.
        assert_eq!(run_ok("function f(o){let {a,b}=o; return a+b} console.log(f({a:3,b:4}))"), vec!["7"]);
        assert_eq!(run_ok("function mk(){let [a,b]=[1,2]; return ()=>a+b} console.log(mk()())"), vec!["3"]);
    }

    #[test]
    fn number_to_radix_and_array_ctor() {
        // Number.toString(radix).
        assert_eq!(run_ok("console.log((255).toString(16), (255).toString(2), (10).toString())"), vec!["ff 11111111 10"]);
        assert_eq!(run_ok("console.log((-42).toString(16), (35).toString(36), (3735928559).toString(16))"), vec!["-2a z deadbeef"]);
        // new Array(n) → n holes; new Array(a,b,…) / Array(...) → the args.
        assert_eq!(run_ok("console.log(new Array(3).length, new Array(3).fill(0).join(','))"), vec!["3 0,0,0"]);
        assert_eq!(run_ok("console.log(new Array(1,2,3).join(','), Array(4,5).join(','))"), vec!["1,2,3 4,5"]);
        assert_eq!(run_ok("console.log(Array(3).fill(7).map((x,i)=>x+i).join(','))"), vec!["7,8,9"]);
        // Invalid length throws a RangeError; new Object()/Object() → {}.
        assert_eq!(run_ok("let e='ok'; try{ new Array(-1); }catch(x){ e='threw'; } console.log(e)"), vec!["threw"]);
        assert_eq!(run_ok("let o=new Object(); o.x=1; console.log(o.x, JSON.stringify(Object()))"), vec!["1 {}"]);
    }

    #[test]
    fn static_builtins() {
        // Array.from over array / string / array-like, with and without a map fn.
        assert_eq!(run_ok("console.log(Array.from([1,2,3],x=>x*2).join(','))"), vec!["2,4,6"]);
        assert_eq!(run_ok("console.log(Array.from({length:3},(_, i)=>i).join(','))"), vec!["0,1,2"]);
        assert_eq!(run_ok("console.log(Array.from('abc').join('-'))"), vec!["a-b-c"]);
        assert_eq!(run_ok("console.log(Array.of(1,2,3).join(','), Array.of(7).length)"), vec!["1,2,3 1"]);
        // Object.assign mutates + returns the target.
        assert_eq!(run_ok("let t={a:1}; let r=Object.assign(t,{a:9,b:2}); console.log(r===t, t.a, t.b)"), vec!["true 9 2"]);
        // String.fromCharCode.
        assert_eq!(run_ok("console.log(String.fromCharCode(72,73,33))"), vec!["HI!"]);
        // Number.isX (no coercion).
        assert_eq!(run_ok("console.log(Number.isInteger(5), Number.isInteger(5.5), Number.isInteger('5'))"), vec!["true false false"]);
        assert_eq!(run_ok("console.log(Number.isSafeInteger(2**53-1), Number.isSafeInteger(2**53))"), vec!["true false"]);
        // Math.max/min spread (incl. mixed plain + spread args).
        assert_eq!(run_ok("let a=[4,2,8,1]; console.log(Math.max(...a), Math.min(...a), Math.max(1,...[5,3],10))"), vec!["8 1 10"]);
        // .at() with negative indexing on arrays and strings.
        assert_eq!(run_ok("console.log([10,20,30].at(-1), [1,2].at(5))"), vec!["30 undefined"]);
        assert_eq!(run_ok("console.log('hello'.at(-1), 'hi'.at(10))"), vec!["o undefined"]);
    }

    #[test]
    fn nullish_and_logical_assign() {
        // ?? keeps the left unless null/undefined (0 and "" are kept).
        assert_eq!(run_ok("console.log(null ?? 5, 0 ?? 9, undefined ?? 'x', '' ?? 'y')"), vec!["5 0 x "]);
        // Logical assignment, short-circuit (RHS not evaluated when skipped).
        assert_eq!(run_ok("let a=0; a||=7; let b=1; b&&=9; console.log(a,b)"), vec!["7 9"]);
        assert_eq!(run_ok("let x=5; x??=10; let y=null; y??=20; console.log(x,y)"), vec!["5 20"]);
        assert_eq!(run_ok("let cnt=0; function f(){cnt++;return 5} let v=1; v||=f(); console.log(v,cnt)"), vec!["1 0"]);
        // Member logical assignment + the counter idiom.
        assert_eq!(run_ok("let o={}; o.a ??= 1; o.a ??= 2; console.log(o.a)"), vec!["1"]);
        assert_eq!(run_ok("let c={}; for(let k of ['a','b','a']){ c[k] ??= 0; c[k]++; } console.log(c.a, c.b)"), vec!["2 1"]);
    }

    #[test]
    fn compound_and_update_assignment() {
        // All arithmetic/bitwise compound operators on a local.
        assert_eq!(run_ok("let a=10; a/=2; a%=3; a**=3; console.log(a)"), vec!["8"]);
        assert_eq!(run_ok("let f=1; f<<=4; f|=1; f&=0xF; f^=2; console.log(f)"), vec!["3"]);
        // Compound + update on members (property and index).
        assert_eq!(run_ok("let o={n:10}; o.n+=5; o.n*=2; console.log(o.n)"), vec!["30"]);
        assert_eq!(run_ok("let a=[1,2,3]; a[0]+=10; a[1]*=3; console.log(a.join(','))"), vec!["11,6,3"]);
        assert_eq!(run_ok("let o={n:5}; let r=[o.n++, o.n, ++o.n]; console.log(r.join(','))"), vec!["5,6,7"]);
        assert_eq!(run_ok("let a=[10,20]; let r=[a[0]++, a[0], --a[1]]; console.log(r.join(','))"), vec!["10,11,19"]);
    }

    #[test]
    fn object_spread_and_computed_keys() {
        assert_eq!(run_ok("let o={a:1,...{b:2,c:3}}; console.log(o.a,o.b,o.c)"), vec!["1 2 3"]);
        // Later properties win over a spread; array source spreads as index keys.
        assert_eq!(run_ok("let base={x:1,y:2}; let o={...base, y:9, z:3}; console.log(o.x,o.y,o.z)"), vec!["1 9 3"]);
        assert_eq!(run_ok("let o={...[10,20]}; console.log(o[0],o[1])"), vec!["10 20"]);
        // null/undefined spread is a no-op.
        assert_eq!(run_ok("let o={...null,...undefined,a:1}; console.log(o.a, Object.keys(o).length)"), vec!["1 1"]);
        // Computed keys, including a template-literal key.
        assert_eq!(run_ok("let k='dyn'; let o={[k]:42,[`a${1}`]:7}; console.log(o.dyn,o.a1)"), vec!["42 7"]);
    }

    #[test]
    fn bitwise_and_exponent() {
        assert_eq!(run_ok("console.log(5 & 3, 5 | 2, 5 ^ 1, ~5)"), vec!["1 7 4 -6"]);
        assert_eq!(run_ok("console.log(1<<4, 256>>2, -8>>1)"), vec!["16 64 -4"]);
        // Unsigned right shift yields a uint32 (can exceed i32::MAX).
        assert_eq!(run_ok("console.log(-1>>>0, (1<<31)>>>0, -1>>>28)"), vec!["4294967295 2147483648 15"]);
        // The canonical (x*31+c)|0 hash idiom.
        assert_eq!(run_ok("let h=0; for(let i=0;i<5;i++) h=(h*31 + i)|0; console.log(h)"), vec!["31810"]);
        // Exponentiation, right-associative.
        assert_eq!(run_ok("console.log(2**10, (-2)**3, 2**3**2, 10**-2)"), vec!["1024 -8 512 0.01"]);
        // Operands coerce via ToInt32 (bool/string/null/undefined/NaN/float).
        assert_eq!(run_ok("console.log(true & 1, '5'|0, null|0, undefined|0, 3.9|0, NaN|0)"), vec!["1 5 0 0 3 0"]);
    }

    #[test]
    fn assignment_destructuring() {
        // The swap idiom + plain array targets.
        assert_eq!(run_ok("let a=1,b=2; [a,b]=[b,a]; console.log(a,b)"), vec!["2 1"]);
        assert_eq!(run_ok("let a,b,c; [a,b,c]=[10,20,30]; console.log(a+b+c)"), vec!["60"]);
        // Rest and defaults in an assignment target.
        assert_eq!(run_ok("let a,r; [a,...r]=[1,2,3,4]; console.log(a, r.join(','))"), vec!["1 2,3,4"]);
        assert_eq!(run_ok("let a,b; [a=5,b=9]=[1]; console.log(a,b)"), vec!["1 9"]);
        // Object assignment destructuring (shorthand, rename, default).
        assert_eq!(run_ok("let x,y; ({x,y}=({x:1,y:2})); console.log(x+y)"), vec!["3"]);
        assert_eq!(run_ok("let p,q; ({a:p,b:q}=({a:7,b:8})); console.log(p,q)"), vec!["7 8"]);
        assert_eq!(run_ok("let x; ({x=42}=({})); console.log(x)"), vec!["42"]);
        // Member targets and nesting.
        assert_eq!(run_ok("let o={}; [o.a,o.b]=[1,2]; console.log(o.a,o.b)"), vec!["1 2"]);
        assert_eq!(run_ok("let a,b,c; [a,[b,c]]=[1,[2,3]]; console.log(a,b,c)"), vec!["1 2 3"]);
        // The assignment expression evaluates to the right-hand side.
        assert_eq!(run_ok("let a,b; let r=([a,b]=[1,2]); console.log(r.join(','))"), vec!["1,2"]);
        // Object rest in an assignment target (own keys minus the siblings).
        assert_eq!(run_ok("let a,rest; ({a,...rest}=({a:1,b:2,c:3})); console.log(a, JSON.stringify(rest))"), vec![r#"1 {"b":2,"c":3}"#]);
        assert_eq!(run_ok("let x,others; ({a:x,...others}=({a:10,p:1,q:2})); console.log(x, JSON.stringify(others))"), vec![r#"10 {"p":1,"q":2}"#]);
        assert_eq!(run_ok("let a,o={}; ({a,...o.bag}=({a:5,m:6})); console.log(a, JSON.stringify(o.bag))"), vec![r#"5 {"m":6}"#]);
    }

    #[test]
    fn labeled_break_continue() {
        // continue label skips to the next iteration of the labeled outer loop.
        assert_eq!(run_ok("let r=[]; outer: for(let i=0;i<3;i++){ for(let j=0;j<3;j++){ if(j===1) continue outer; r.push(i+''+j); } } console.log(r.join(','))"), vec!["00,10,20"]);
        // break label exits the labeled outer loop entirely.
        assert_eq!(run_ok("let r=[]; outer: for(let i=0;i<3;i++){ for(let j=0;j<3;j++){ if(i===1&&j===1) break outer; r.push(i+''+j); } } console.log(r.join(','))"), vec!["00,01,02,10"]);
        // Works over for-of, and with a labeled break inside nested labels.
        assert_eq!(run_ok("let r=[]; loop: for(let x of [1,2,3]){ for(let y of [10,20]){ if(y===20) continue loop; r.push(x*y); } } console.log(r.join(','))"), vec!["10,20,30"]);
        assert_eq!(run_ok("let r=[]; a: for(let i=0;i<2;i++) b: for(let j=0;j<3;j++){ if(j===2) break a; r.push(j); } console.log(r.join(','))"), vec!["0,1"]);
        // A label on a block makes `break label` exit the block.
        assert_eq!(run_ok("let r=[]; blk:{ r.push(1); break blk; r.push(2); } console.log(r.join(','))"), vec!["1"]);
    }

    #[test]
    fn for_of_for_in_capture() {
        // A closure capturing a for-of / for-in loop variable resolves it (was a
        // pre-existing bug: the loop var wasn't detected as captured → not boxed).
        // Within-iteration capture+use matches node exactly.
        assert_eq!(run_ok("let out=[]; for(let x of [1,2,3]){ let g=()=>x*10; out.push(g()); } console.log(out.join(','))"), vec!["10,20,30"]);
        assert_eq!(run_ok("let out=[]; for(let k in {a:1,b:2}){ out.push((()=>k)()); } console.log(out.join(','))"), vec!["a,b"]);
        assert_eq!(run_ok("function f(){let r=[]; for(let v of [10,20]){ r.push((()=>v)()); } return r} console.log(f().join(','))"), vec!["10,20"]);
        // Generator loop var captured within the iteration.
        assert_eq!(run_ok("function* g(){yield 1;yield 2} let o=[]; for(let n of g()){ o.push((()=>n+100)()); } console.log(o.join(','))"), vec!["101,102"]);
    }

    #[test]
    fn for_of_destructuring() {
        assert_eq!(run_ok("let r=[]; for(let [a,b] of [[1,2],[3,4]]) r.push(a+b); console.log(r.join(','))"), vec!["3,7"]);
        // The canonical Object.entries idiom.
        assert_eq!(run_ok("let o={x:1,y:2}; let r=[]; for(let [k,v] of Object.entries(o)) r.push(k+'='+v); console.log(r.join(' '))"), vec!["x=1 y=2"]);
        assert_eq!(run_ok("let r=[]; for(let {n} of [{n:'a'},{n:'b'}]) r.push(n); console.log(r.join(''))"), vec!["ab"]);
        // Rest and defaults in the head.
        assert_eq!(run_ok("let r=[]; for(let [a,...t] of [[1,2,3]]) r.push(a+':'+t.join(',')); console.log(r[0])"), vec!["1:2,3"]);
        assert_eq!(run_ok("let r=[]; for(let {a,b=9} of [{a:1,b:2},{a:3}]) r.push(a+''+b); console.log(r.join(' '))"), vec!["12 39"]);
        // Captured destructured loop var.
        assert_eq!(run_ok("let f; for(let [a,b] of [[1,2]]) f=()=>a+b; console.log(f())"), vec!["3"]);
    }

    #[test]
    fn function_inspect_label() {
        // Named functions / methods show their name; truly anonymous ones don't.
        assert_eq!(run_ok("function foo(){} console.log(foo)"), vec!["[Function: foo]"]);
        assert_eq!(run_ok("console.log([function named(){}, x=>x])"), vec!["[ [Function: named], [Function (anonymous)] ]"]);
        assert_eq!(run_ok("class A{m(){}} console.log(new A().m)"), vec!["[Function: m]"]);
    }

    #[test]
    fn function_name_and_length() {
        // .name: declaration, named expression, class, and inference for an
        // anonymous arrow / function expression bound to a variable.
        assert_eq!(run_ok("function foo(){} console.log(foo.name)"), vec!["foo"]);
        assert_eq!(run_ok("let q=function named(){}; console.log(q.name)"), vec!["named"]);
        assert_eq!(run_ok("const baz=()=>{}; console.log(baz.name)"), vec!["baz"]);
        assert_eq!(run_ok("const bar=function(){}; console.log(bar.name)"), vec!["bar"]);
        assert_eq!(run_ok("class C{} console.log(C.name)"), vec!["C"]);
        // A truly anonymous function (in an array) has an empty name.
        assert_eq!(run_ok("console.log([x=>x][0].name === '')"), vec!["true"]);
        // .length: declared parameter count (rest excluded).
        assert_eq!(run_ok("function f(a,b,c){} console.log(f.length, ((x,y)=>{}).length, (()=>{}).length)"), vec!["3 2 0"]);
        assert_eq!(run_ok("function r(a,...rest){} console.log(r.length)"), vec!["1"]);
        assert_eq!(run_ok("class C{constructor(a,b){}} console.log(C.length)"), vec!["2"]);
    }

    #[test]
    fn promises() {
        // resolve/reject + then/catch; chaining; a throw in then routes to catch.
        assert_eq!(run_ok("Promise.resolve(5).then(v=>console.log('got',v))"), vec!["got 5"]);
        assert_eq!(run_ok("Promise.reject('e').catch(e=>console.log('caught',e))"), vec!["caught e"]);
        assert_eq!(run_ok("Promise.resolve(1).then(v=>v+1).then(v=>console.log(v))"), vec!["2"]);
        assert_eq!(run_ok("Promise.resolve(1).then(v=>{throw 'x'}).catch(e=>console.log('c:'+e))"), vec!["c:x"]);
        // The defining ordering property: reactions run as microtasks AFTER sync.
        assert_eq!(run_ok("console.log('A'); Promise.resolve().then(()=>console.log('C')); console.log('B')"), vec!["A", "B", "C"]);
        // new Promise: resolve, reject, chaining, and adopting a returned promise.
        assert_eq!(run_ok("new Promise(res=>res(42)).then(v=>console.log(v))"), vec!["42"]);
        assert_eq!(run_ok("new Promise((res,rej)=>rej('bad')).catch(e=>console.log('err',e))"), vec!["err bad"]);
        assert_eq!(run_ok("new Promise(r=>r(Promise.resolve(99))).then(v=>console.log(v))"), vec!["99"]);
        // A promise resolved later by a stored resolver.
        assert_eq!(run_ok("let r; let p=new Promise(res=>{r=res}); p.then(v=>console.log('late',v)); r(7)"), vec!["late 7"]);
        // The executor captures an outer variable (regression: capture analysis
        // must descend into `new` arguments to box `v`).
        assert_eq!(run_ok("function delay(v){return new Promise(res=>res(v))} delay(9).then(x=>console.log('d',x))"), vec!["d 9"]);
        // finally runs on both paths and passes the value/reason through.
        assert_eq!(run_ok("Promise.resolve(1).finally(()=>console.log('cleanup')).then(v=>console.log('v',v))"), vec!["cleanup", "v 1"]);
        assert_eq!(run_ok("console.log(typeof Promise.resolve(1))"), vec!["object"]);
    }

    #[test]
    fn promise_combinators() {
        // all: array of values (mixed plain + promise); first rejection wins; empty.
        assert_eq!(run_ok("Promise.all([1,Promise.resolve(2),3]).then(a=>console.log(a.join(',')))"), vec!["1,2,3"]);
        assert_eq!(run_ok("Promise.all([Promise.resolve(1),Promise.reject('x'),Promise.resolve(3)]).catch(e=>console.log('r',e))"), vec!["r x"]);
        assert_eq!(run_ok("Promise.all([]).then(a=>console.log(a.length))"), vec!["0"]);
        // race: first to settle (fulfil or reject).
        assert_eq!(run_ok("Promise.race([Promise.resolve('fast'),Promise.reject('slow')]).then(v=>console.log(v))"), vec!["fast"]);
        assert_eq!(run_ok("Promise.race([Promise.reject('boom'),Promise.resolve('ok')]).catch(e=>console.log('r',e))"), vec!["r boom"]);
        // allSettled: status records on both paths.
        assert_eq!(
            run_ok("Promise.allSettled([Promise.resolve(1),Promise.reject('e')]).then(rs=>console.log(rs.map(r=>r.status+(r.status==='fulfilled'?r.value:r.reason)).join(',')))"),
            vec!["fulfilled1,rejectede"]
        );
        // any: first fulfilment; all-reject → AggregateError; empty → AggregateError.
        assert_eq!(run_ok("Promise.any([Promise.reject('a'),Promise.resolve('win')]).then(v=>console.log(v))"), vec!["win"]);
        assert_eq!(run_ok("Promise.any([Promise.reject('e1'),Promise.reject('e2')]).catch(e=>console.log(e.name,e.errors.join(',')))"), vec!["AggregateError e1,e2"]);
        assert_eq!(run_ok("Promise.any([]).catch(e=>console.log(e.name))"), vec!["AggregateError"]);
        // Integrates with await + destructuring.
        assert_eq!(run_ok("async function f(){let [a,b]=await Promise.all([Promise.resolve(10),Promise.resolve(20)]); return a+b} f().then(v=>console.log(v))"), vec!["30"]);
    }

    #[test]
    fn generators() {
        // Manual next(): values then done; return value reported once.
        assert_eq!(run_ok("function* g(){yield 1;yield 2} let it=g(); console.log(it.next().value,it.next().value,it.next().done)"), vec!["1 2 true"]);
        assert_eq!(run_ok("function* g(){yield 1; return 9} let it=g(); console.log(JSON.stringify(it.next()),JSON.stringify(it.next()),JSON.stringify(it.next()))"), vec![r#"{"value":1,"done":false} {"value":9,"done":true} {"done":true}"#]);
        // Empty generator; value sent into a yield expression.
        assert_eq!(run_ok("function* g(){} console.log(g().next().done)"), vec!["true"]);
        assert_eq!(run_ok("function* g(){let x=yield 1; yield x+10} let it=g(); console.log(it.next().value, it.next(5).value)"), vec!["1 15"]);
        // for-of over a generator: direct call AND via a variable.
        assert_eq!(run_ok("function* g(){yield 1;yield 2;yield 3} let s=0; for(let x of g()) s+=x; console.log(s)"), vec!["6"]);
        assert_eq!(run_ok("function* g(){yield 1;yield 2} let gen=g(); let r=[]; for(let x of gen) r.push(x); console.log(r.join(','))"), vec!["1,2"]);
        // for-of destructuring a generator's elements.
        assert_eq!(run_ok("function* g(){yield [1,2]; yield [3,4]} let r=[]; for(let [a,b] of g()) r.push(a+b); console.log(r.join(','))"), vec!["3,7"]);
        // Infinite generator with break terminates (lazy pull).
        assert_eq!(run_ok("function* nat(){let i=0; while(true) yield i++} let r=[]; for(let x of nat()){ if(x>=4) break; r.push(x); } console.log(r.join(','))"), vec!["0,1,2,3"]);
        // Spread and Array.from drain a finite generator.
        assert_eq!(run_ok("function* g(){yield 1;yield 2;yield 3} console.log([...g()].join('-'), Array.from(g()).length)"), vec!["1-2-3 3"]);
        // A generator using a captured outer variable + a range helper.
        assert_eq!(run_ok("function* range(n){for(let i=0;i<n;i++) yield i*i} console.log([...range(4)].join(','))"), vec!["0,1,4,9"]);
        // typeof and inspect.
        assert_eq!(run_ok("function* g(){} console.log(typeof g, typeof g())"), vec!["function object"]);
    }

    #[test]
    fn generator_methods_and_yield_star() {
        // Object and class generator methods (incl. using `this` and static).
        assert_eq!(run_ok("let o={*gen(){yield 1;yield 2}}; console.log([...o.gen()].join(','))"), vec!["1,2"]);
        assert_eq!(run_ok("class C{*vals(){yield 1;yield 2}} console.log([...new C().vals()].join(','))"), vec!["1,2"]);
        assert_eq!(run_ok("class C{constructor(){this.xs=[10,20,30]} *each(){for(let x of this.xs) yield x}} console.log([...new C().each()].join(','))"), vec!["10,20,30"]);
        assert_eq!(run_ok("class C{static *make(){yield 1;yield 2}} console.log([...C.make()].join(','))"), vec!["1,2"]);
        // yield* delegation over a generator, array, string, and nested.
        assert_eq!(run_ok("function* inner(){yield 1;yield 2} function* outer(){yield* inner(); yield 3} console.log([...outer()].join(','))"), vec!["1,2,3"]);
        assert_eq!(run_ok("function* g(){yield* [1,2,3]; yield* 'ab'} console.log([...g()].join(','))"), vec!["1,2,3,a,b"]);
        assert_eq!(run_ok("function* g(){yield 0; yield* [1,2]; yield 3} console.log([...g()].join(','))"), vec!["0,1,2,3"]);
        assert_eq!(run_ok("function* nest(){yield* (function*(){yield* [1,2]})()} console.log([...nest()].join(','))"), vec!["1,2"]);
    }

    #[test]
    fn async_await() {
        // An async function returns a Promise; its body's `return` fulfills it.
        assert_eq!(run_ok("async function f(){return 1} f().then(v=>console.log('v',v))"), vec!["v 1"]);
        // await a non-promise (still yields a microtask tick) and a real promise.
        assert_eq!(run_ok("async function f(){let x=await 5; return x+10} f().then(v=>console.log(v))"), vec!["15"]);
        assert_eq!(run_ok("async function f(){let x=await Promise.resolve(3); let y=await Promise.resolve(4); return x*y} f().then(v=>console.log(v))"), vec!["12"]);
        // Rejection caught by try/catch around the await.
        assert_eq!(run_ok("async function f(){try{await Promise.reject('boom'); return 'no'}catch(e){return 'caught '+e}} f().then(v=>console.log(v))"), vec!["caught boom"]);
        // Uncaught rejection / a thrown body error reject the returned promise.
        assert_eq!(run_ok("async function f(){await Promise.reject('k')} f().catch(e=>console.log('c',e))"), vec!["c k"]);
        assert_eq!(run_ok("async function f(){throw new Error('x')} f().catch(e=>console.log(e.message))"), vec!["x"]);
        // Ordering: sync runs first; the await suspends and resumes as a microtask.
        assert_eq!(
            run_ok("console.log('start'); async function f(){console.log('before'); await 0; console.log('after')} f(); console.log('end')"),
            vec!["start", "before", "end", "after"]
        );
        // Async calling async + await in a loop, accumulating.
        assert_eq!(
            run_ok("async function dbl(n){return n*2} async function f(){let t=0; for(let i=1;i<=3;i++){t+=await dbl(i)} return t} f().then(v=>console.log(v))"),
            vec!["12"]
        );
        // await a `new Promise` that resolves synchronously with a captured value.
        assert_eq!(
            run_ok("function delay(v){return new Promise(res=>res(v))} async function f(){let a=await delay(10); let b=await delay(20); return a+b} f().then(v=>console.log(v))"),
            vec!["30"]
        );
        // Async arrow.
        assert_eq!(run_ok("const f=async()=>(await Promise.resolve(7))+1; f().then(v=>console.log(v))"), vec!["8"]);
        // typeof of an async call is the Promise object.
        assert_eq!(run_ok("async function f(){} console.log(typeof f())"), vec!["object"]);
        // try/finally around `return await` runs the finally before fulfilling.
        assert_eq!(
            run_ok("async function f(){try{return await Promise.resolve('ok')}finally{console.log('fin')}} f().then(v=>console.log(v))"),
            vec!["fin", "ok"]
        );
        // A rejection thrown in at the await still runs the finally on the way out.
        assert_eq!(
            run_ok("async function f(){try{await Promise.reject('e')}finally{console.log('fin')}} f().catch(e=>console.log('c',e))"),
            vec!["fin", "c e"]
        );
    }

    #[test]
    fn map_basics() {
        assert_eq!(run_ok("let m=new Map(); m.set('a',1).set('b',2); console.log(m.get('a'),m.get('b'),m.size,m.has('a'),m.has('z'))"), vec!["1 2 2 true false"]);
        assert_eq!(run_ok("let m=new Map([['x',10],['y',20]]); console.log(m.get('x'),m.get('y'),m.size)"), vec!["10 20 2"]);
        // set on an existing key updates in place (one entry); delete returns bool.
        assert_eq!(run_ok("let m=new Map(); m.set(1,'a'); m.set(1,'b'); console.log(m.get(1),m.size)"), vec!["b 1"]);
        assert_eq!(run_ok("let m=new Map([[1,1]]); console.log(m.delete(1),m.delete(1),m.size)"), vec!["true false 0"]);
        // Iteration: for-of entries, keys/values, forEach(value,key), spread.
        assert_eq!(run_ok("let m=new Map([['a',1],['b',2]]); let r=[]; for(let [k,v] of m) r.push(k+v); console.log(r.join(','))"), vec!["a1,b2"]);
        assert_eq!(run_ok("let m=new Map([['a',1],['b',2]]); console.log([...m.keys()].join(','), [...m.values()].join(','))"), vec!["a,b 1,2"]);
        assert_eq!(run_ok("let m=new Map([['a',1]]); let r=[]; m.forEach((v,k)=>r.push(k+'='+v)); console.log(r.join(','))"), vec!["a=1"]);
        // SameValueZero keys: NaN dedupes, -0/+0 collapse, objects by identity, no coercion.
        assert_eq!(run_ok("let m=new Map(); m.set(NaN,1).set(NaN,2); console.log(m.size,m.get(NaN))"), vec!["1 2"]);
        assert_eq!(run_ok("let m=new Map(); m.set(-0,'z'); console.log(m.get(0), m.has(0))"), vec!["z true"]);
        assert_eq!(run_ok("let m=new Map(); m.set(1,'n'); console.log(m.get('1'))"), vec!["undefined"]);
        // console.log + JSON shape.
        assert_eq!(run_ok("console.log(new Map([['a',1],['b',2]]))"), vec!["Map(2) { 'a' => 1, 'b' => 2 }"]);
        assert_eq!(run_ok("console.log(JSON.stringify({m:new Map([[1,2]])}))"), vec![r#"{"m":{}}"#]);
    }

    #[test]
    fn set_basics() {
        assert_eq!(run_ok("let s=new Set([1,2,2,3]); console.log(s.size, s.has(2), s.has(9))"), vec!["3 true false"]);
        assert_eq!(run_ok("let s=new Set(); s.add(1).add(2).add(1); console.log(s.size, [...s].join(','))"), vec!["2 1,2"]);
        assert_eq!(run_ok("let s=new Set([1,2,3]); console.log(s.delete(2), s.size, [...s].join(','))"), vec!["true 2 1,3"]);
        // Set from a string iterates chars (deduped).
        assert_eq!(run_ok("let s=new Set('aabbc'); console.log(s.size, [...s].join(''))"), vec!["3 abc"]);
        // for-of yields values; forEach; NaN dedupe.
        assert_eq!(run_ok("let r=[]; for(let v of new Set([10,20])) r.push(v); console.log(r.join(','))"), vec!["10,20"]);
        assert_eq!(run_ok("let s=new Set([1,2,3]); let t=0; s.forEach(v=>t+=v); console.log(t)"), vec!["6"]);
        assert_eq!(run_ok("console.log(new Set([NaN,NaN]).size)"), vec!["1"]);
        // The canonical dedupe idiom + console.log.
        assert_eq!(run_ok("console.log([...new Set([3,1,3,2,1])].join(','))"), vec!["3,1,2"]);
        assert_eq!(run_ok("console.log(new Set([1,2]))"), vec!["Set(2) { 1, 2 }"]);
    }

    #[test]
    fn classes() {
        // Constructor + method + this.
        assert_eq!(run_ok("class A{constructor(x){this.x=x} get(){return this.x}} console.log(new A(5).get())"), vec!["5"]);
        // Class fields (with and without initializers) + field mutation.
        assert_eq!(run_ok("class C{count=0; inc(){this.count++; return this.count}} let c=new C(); console.log(c.inc(), c.inc())"), vec!["1 2"]);
        // Method chaining via `return this`; method calling another method.
        assert_eq!(run_ok("class K{constructor(){this.v=0} add(n){this.v+=n;return this} val(){return this.v}} console.log(new K().add(3).add(4).val())"), vec!["7"]);
        assert_eq!(run_ok("class A{constructor(n){this.n=n} d(){return this.n*2} q(){return this.d()*2}} console.log(new A(5).q())"), vec!["20"]);
        // A constructor returning an object replaces the instance.
        assert_eq!(run_ok("class W{constructor(){return {custom:true}}} console.log(new W().custom)"), vec!["true"]);
        // Methods are non-enumerable: keys/JSON show only fields.
        assert_eq!(run_ok("class A{constructor(){this.k=1;this.j=2} m(){}} let a=new A(); console.log(Object.keys(a).join(','), JSON.stringify(a))"), vec![r#"k,j {"k":1,"j":2}"#]);
        // instanceof for user classes; typeof a class is "function".
        assert_eq!(run_ok("class A{} class B{} let a=new A(); console.log(a instanceof A, a instanceof B, typeof A)"), vec!["true false function"]);
        // Arrays of instances; console.log prints the constructor name.
        assert_eq!(run_ok("class Pt{constructor(x){this.x=x}} console.log([new Pt(1),new Pt(2)].map(p=>p.x).join(','))"), vec!["1,2"]);
        assert_eq!(run_ok("class Pt{constructor(x,y){this.x=x;this.y=y}} console.log(new Pt(3,4))"), vec!["Pt { x: 3, y: 4 }"]);
        // Getters: invoked on read (this = instance), not enumerable.
        assert_eq!(run_ok("class C{constructor(){this.items=[1,2,3]} get size(){return this.items.length}} console.log(new C().size)"), vec!["3"]);
        assert_eq!(run_ok("class C{constructor(){this.n=1} get d(){return this.n*2}} let c=new C(); console.log(c.d, Object.keys(c).join(','))"), vec!["2 n"]);
        // Setters: invoked on write; get/set pair; setter-only; inherited; own
        // data property still shadows.
        assert_eq!(run_ok("class T{constructor(c){this._c=c} get c(){return this._c} set c(v){this._c=v*2}} let t=new T(5); console.log(t.c); t.c=10; console.log(t.c)"), vec!["5", "20"]);
        assert_eq!(run_ok("class L{set m(v){this.last='['+v+']'}} let l=new L(); l.m='hi'; console.log(l.last)"), vec!["[hi]"]);
        assert_eq!(run_ok("class B{set v(x){this._v=x*2} get v(){return this._v}} class D extends B{} let d=new D(); d.v=21; console.log(d.v)"), vec!["42"]);
        assert_eq!(run_ok("class P{constructor(){this.x=1}} let p=new P(); p.x=5; p.y=9; console.log(p.x,p.y)"), vec!["5 9"]);
        // Static methods + fields; instances don't see statics.
        assert_eq!(run_ok("class M{static sq(n){return n*n}} console.log(M.sq(5))"), vec!["25"]);
        assert_eq!(run_ok("class Cfg{static V='1.0'; static MAX=100} console.log(Cfg.V, Cfg.MAX)"), vec!["1.0 100"]);
        assert_eq!(run_ok("class C{static n=0; constructor(){C.n++; this.id=C.n}} let a=new C(),b=new C(); console.log(a.id,b.id,C.n)"), vec!["1 2 2"]);
        assert_eq!(run_ok("class A{static s(){return 1}} let a=new A(); console.log(typeof a.s, typeof A.s)"), vec!["undefined function"]);
    }

    #[test]
    fn class_inheritance() {
        // Inherited method; instanceof up the chain.
        assert_eq!(run_ok("class A{m(){return 1}} class B extends A{} let b=new B(); console.log(b.m(), b instanceof A, b instanceof B)"), vec!["1 true true"]);
        // super(args) in a derived constructor; fields after super.
        assert_eq!(run_ok("class A{constructor(x){this.x=x}} class B extends A{constructor(x,y){super(x);this.y=y}} let b=new B(3,4); console.log(b.x,b.y)"), vec!["3 4"]);
        // Implicit super forwards constructor args.
        assert_eq!(run_ok("class A{constructor(n){this.n=n}} class B extends A{} console.log(new B(7).n)"), vec!["7"]);
        // super.method() and override.
        assert_eq!(run_ok("class A{g(){return 'A'}} class B extends A{g(){return 'B->'+super.g()}} console.log(new B().g())"), vec!["B->A"]);
        assert_eq!(run_ok("class Animal{constructor(n){this.name=n} speak(){return this.name+' sound'}} class Dog extends Animal{speak(){return this.name+' barks'}} console.log(new Dog('Rex').speak())"), vec!["Rex barks"]);
        // Inherited fields; 3-level chain.
        assert_eq!(run_ok("class A{x=1} class B extends A{y=2} let b=new B(); console.log(b.x,b.y)"), vec!["1 2"]);
        assert_eq!(run_ok("class A{constructor(){this.t='a'}} class B extends A{} class C extends B{} console.log(new C().t, new C() instanceof A)"), vec!["a true"]);
        // Inherited static method.
        assert_eq!(run_ok("class A{static make(){return 'A'}} class B extends A{} console.log(B.make())"), vec!["A"]);
    }

    #[test]
    fn instanceof_operator() {
        // Built-in collections / functions.
        assert_eq!(run_ok("console.log([] instanceof Array, [] instanceof Object)"), vec!["true true"]);
        assert_eq!(run_ok("console.log(({}) instanceof Object, ({}) instanceof Array)"), vec!["true false"]);
        assert_eq!(run_ok("let f=x=>x; console.log(f instanceof Function, f instanceof Object)"), vec!["true true"]);
        // Primitives are never instances.
        assert_eq!(run_ok("console.log(5 instanceof Object, 's' instanceof Object, null instanceof Object)"), vec!["false false false"]);
        // Error hierarchy: a subtype is also an Error; siblings don't match.
        assert_eq!(run_ok("let e=new TypeError('x'); console.log(e instanceof TypeError, e instanceof Error, e instanceof RangeError)"), vec!["true true false"]);
        // Engine-thrown errors are real Error objects (name/message/instanceof).
        assert_eq!(run_ok("try{null.x}catch(e){console.log(e instanceof TypeError, e.name)}"), vec!["true TypeError"]);
        assert_eq!(run_ok("try{let a=[];a.length=-1}catch(e){console.log(e instanceof RangeError)}"), vec!["true"]);
    }

    #[test]
    fn is_nan_is_finite() {
        assert_eq!(run_ok("console.log(isNaN(NaN), isNaN(5), isNaN('x'), isNaN('12'))"), vec!["true false true false"]);
        assert_eq!(run_ok("console.log(isFinite(5), isFinite(Infinity), isFinite(NaN), isFinite('3'))"), vec!["true false false true"]);
    }

    #[test]
    fn destructuring_parameters() {
        // The common .map(([k,v])=>…) over entries; arrow object-pattern param.
        assert_eq!(run_ok("console.log(Object.entries({a:1,b:2}).map(([k,v])=>k+v).join(','))"), vec!["a1,b2"]);
        assert_eq!(run_ok("let f=({x,y})=>x+y; console.log(f({x:3,y:4}))"), vec!["7"]);
        // Function with mixed array + object pattern params.
        assert_eq!(run_ok("function f([a,b],{c}){return a+b+c} console.log(f([1,2],{c:3}))"), vec!["6"]);
        // Defaults and rest inside a pattern parameter.
        assert_eq!(run_ok("let f=({a,b=10})=>a+b; console.log(f({a:1}), f({a:1,b:2}))"), vec!["11 3"]);
        assert_eq!(run_ok("let f=([a,...rest])=>a+':'+rest.join(','); console.log(f([1,2,3,4]))"), vec!["1:2,3,4"]);
        // forEach with a pattern param; pattern param captured by a closure.
        assert_eq!(run_ok("let r=[]; [[1,2],[3,4]].forEach(([a,b])=>r.push(a+b)); console.log(r.join(','))"), vec!["3,7"]);
        assert_eq!(run_ok("let fns=[[1,2],[3,4]].map(([a,b])=>()=>a+b); console.log(fns[0](),fns[1]())"), vec!["3 7"]);
    }

    #[test]
    fn rest_parameters() {
        // Pure rest, rest after fixed params, empty rest.
        assert_eq!(run_ok("function f(...a){return a.length} console.log(f(1,2,3))"), vec!["3"]);
        assert_eq!(run_ok("function f(a,...b){return a+':'+b.join(',')} console.log(f(1,2,3,4))"), vec!["1:2,3,4"]);
        assert_eq!(run_ok("function f(a,...b){return b.length} console.log(f(1))"), vec!["0"]);
        // Arrow rest.
        assert_eq!(run_ok("let g=(...xs)=>xs.reduce((a,b)=>a+b,0); console.log(g(1,2,3,4))"), vec!["10"]);
        // Rest fed by spread (the two halves compose).
        assert_eq!(run_ok("function f(...a){return a.join(',')} console.log(f(...[1,2,3],4))"), vec!["1,2,3,4"]);
        // Rest array captured by an inner closure (boxed into a cell).
        assert_eq!(run_ok("function f(...a){return ()=>a.length} console.log(f(1,2,3)())"), vec!["3"]);
        // Rest method keeps `this`.
        assert_eq!(run_ok("let o={n:5,f(...xs){return this.n+xs.length}}; console.log(o.f(1,2))"), vec!["7"]);
    }

    #[test]
    fn in_operator_and_more_methods() {
        // `in`: own object keys, array indices/length, class-instance inherited
        // methods, Map/Set size. (Plain-object Object.prototype methods aren't
        // inherited here — no prototype chain.)
        assert_eq!(run_ok("let o={a:1,b:2}; console.log('a' in o, 'c' in o)"), vec!["true false"]);
        assert_eq!(run_ok("console.log(0 in [1,2], 5 in [1,2], 'length' in [])"), vec!["true false true"]);
        assert_eq!(run_ok("class A{m(){}} let a=new A(); a.x=1; console.log('m' in a, 'x' in a, 'y' in a)"), vec!["true true false"]);
        assert_eq!(run_ok("class A{am(){}} class B extends A{} console.log('am' in new B())"), vec!["true"]);
        assert_eq!(run_ok("console.log('size' in new Map())"), vec!["true"]);
        // reduceRight (with and without an initial value).
        assert_eq!(run_ok("console.log([1,2,3].reduceRight((a,b)=>a+'-'+b))"), vec!["3-2-1"]);
        assert_eq!(run_ok("console.log([[0,1],[2,3]].reduceRight((a,b)=>a.concat(b),[]).join(','))"), vec!["2,3,0,1"]);
        // Object.fromEntries from an array of pairs and from a Map.
        assert_eq!(run_ok("console.log(JSON.stringify(Object.fromEntries([['a',1],['b',2]])))"), vec![r#"{"a":1,"b":2}"#]);
        assert_eq!(run_ok("let m=new Map([['x',1]]); console.log(Object.fromEntries(m).x)"), vec!["1"]);
    }

    #[test]
    fn array_string_methods_batch2() {
        // flatMap (map + flatten one level; empty array => filter out).
        assert_eq!(run_ok("console.log([1,2,3].flatMap(x=>[x,x*2]).join(','))"), vec!["1,2,2,4,3,6"]);
        assert_eq!(run_ok("console.log([1,2,3].flatMap(x=>x%2?[x]:[]).join(','))"), vec!["1,3"]);
        // Immutable toSorted / toReversed leave the receiver unchanged.
        assert_eq!(run_ok("let a=[3,1,2]; let b=a.toSorted((x,y)=>x-y); console.log(b.join(','), a.join(','))"), vec!["1,2,3 3,1,2"]);
        assert_eq!(run_ok("let a=[1,2,3]; console.log(a.toReversed().join(','), a.join(','))"), vec!["3,2,1 1,2,3"]);
        // findLast / findLastIndex.
        assert_eq!(run_ok("console.log([1,2,3,4].findLast(x=>x<3), [1,2,3,4].findLastIndex(x=>x<3))"), vec!["2 1"]);
        // splice: remove+insert (returns removed), insert-only, negative start.
        assert_eq!(run_ok("let a=[1,2,3,4,5]; let r=a.splice(1,2,9,9,9); console.log(r.join(','), a.join(','))"), vec!["2,3 1,9,9,9,4,5"]);
        assert_eq!(run_ok("let a=[1,2,3]; a.splice(1,0,'x'); console.log(a.join(','))"), vec!["1,x,2,3"]);
        assert_eq!(run_ok("let a=[1,2,3]; console.log(a.splice(-1).join(','), a.join(','))"), vec!["3 1,2"]);
        // String indexOf honors a start position; codePointAt.
        assert_eq!(run_ok("console.log('abcabc'.indexOf('c',3), 'abcabc'.indexOf('a',1))"), vec!["5 3"]);
        assert_eq!(run_ok("console.log('Hello'.codePointAt(0), 'Hi'.codePointAt(5))"), vec!["72 undefined"]);
    }

    #[test]
    fn array_methods_more() {
        assert_eq!(run_ok("let a=[1,2,3]; a.reverse(); console.log(a.join(','))"), vec!["3,2,1"]);
        assert_eq!(run_ok("console.log([1,2].concat([3,4],5,[6]).join(','))"), vec!["1,2,3,4,5,6"]);
        assert_eq!(run_ok("console.log([1,[2,[3]]].flat().length, [1,[2,[3]]].flat(2).join(','))"), vec!["3 1,2,3"]);
        assert_eq!(run_ok("console.log([1,2,3,4].fill(9,1,3).join(','), [1,2,1].lastIndexOf(1))"), vec!["1,9,9,4 2"]);
    }

    #[test]
    fn array_callback_search_methods() {
        assert_eq!(
            run_ok("let a=[1,2,3,4]; console.log(a.find(x=>x>2), a.findIndex(x=>x>2), a.some(x=>x>3), a.every(x=>x>0))"),
            vec!["3 2 true true"],
        );
        assert_eq!(
            run_ok("let a=[1,2,3]; console.log(a.find(x=>x>9), a.findIndex(x=>x>9), a.some(x=>x>9), a.every(x=>x>1))"),
            vec!["undefined -1 false false"],
        );
        // Empty array: some→false, every→true (vacuous truth).
        assert_eq!(run_ok("console.log([].some(x=>x), [].every(x=>x))"), vec!["false true"]);
    }

    #[test]
    fn string_methods_extra() {
        assert_eq!(run_ok("console.log('  hi  '.trim(), 'abc'.startsWith('ab'), 'abc'.endsWith('bc'))"), vec!["hi true true"]);
        assert_eq!(run_ok("console.log('5'.padStart(3,'0'), '5'.padEnd(3,'-'), 'abc'.padStart(2))"), vec!["005 5-- abc"]);
        // replace = first occurrence; replaceAll = all.
        assert_eq!(run_ok("console.log('aXbXc'.replace('X','-'), 'aXbXc'.replaceAll('X','-'))"), vec!["a-bXc a-b-c"]);
    }

    #[test]
    fn math_functions() {
        assert_eq!(
            run_ok("console.log(Math.sqrt(16), Math.floor(3.7), Math.ceil(3.2), Math.abs(-5), Math.trunc(-4.7))"),
            vec!["4 3 4 5 -4"],
        );
        // JS Math.round is half-up (≠ Rust's half-away-from-zero for negatives).
        assert_eq!(run_ok("console.log(Math.round(2.5), Math.round(-2.5), Math.round(-2.6))"), vec!["3 -2 -3"]);
        assert_eq!(
            run_ok("console.log(Math.min(3,1,2), Math.max(1,9,2), Math.pow(2,10), Math.hypot(3,4))"),
            vec!["1 9 1024 5"],
        );
        // sign preserves 0 / maps NaN→NaN; min/max are NaN-sticky; empty → ±Infinity.
        assert_eq!(
            run_ok("console.log(Math.sign(-3), Math.sign(0), Math.sign(NaN), Math.max(1,NaN), Math.max(), Math.min())"),
            vec!["-1 0 NaN NaN -Infinity Infinity"],
        );
        // Argument coercion (string → number).
        assert_eq!(run_ok("console.log(Math.sqrt('9'))"), vec!["3"]);
        // Math.random(): always in [0,1); a dice roll lands in range.
        assert_eq!(run_ok("let ok=true; for(let i=0;i<500;i++){let r=Math.random(); if(!(r>=0&&r<1))ok=false} console.log(ok)"), vec!["true"]);
        assert_eq!(run_ok("let d=Math.floor(Math.random()*6)+1; console.log(d>=1&&d<=6)"), vec!["true"]);
    }

    #[test]
    fn template_literals() {
        assert_eq!(run_ok("let x=5; console.log(`val=${x+1}`)"), vec!["val=6"]);
        assert_eq!(run_ok("let a='A',b=2; console.log(`${a}-${b}-${a+b}`)"), vec!["A-2-A2"]);
        assert_eq!(run_ok("let o={n:7}; console.log(`obj ${o.n} arr ${[1,2].length}`)"), vec!["obj 7 arr 2"]);
        assert_eq!(run_ok("console.log(`no interp`)"), vec!["no interp"]);
        assert_eq!(run_ok("let n=10; let f=()=>`n=${n}`; console.log(f())"), vec!["n=10"]);
    }

    #[test]
    fn tagged_templates() {
        // The tag gets the cooked strings array + the interpolated values.
        assert_eq!(run_ok("function t(s,...v){return s.join('|')+'#'+v.join(',')} console.log(t`a${1}b${2}c`)"), vec!["a|b|c#1,2"]);
        // No interpolations: one string, no values.
        assert_eq!(run_ok("function t(s,...v){return s.join('|')+'#'+v.length} console.log(t`hi`)"), vec!["hi#0"]);
        // `.raw` is the un-escaped parts (here `\\n` stays literal).
        assert_eq!(run_ok(r"function t(s){return s.raw[0]} console.log(t`a\nb`)"), vec![r"a\nb"]);
        // String.raw built-in.
        assert_eq!(run_ok(r"console.log(String.raw`a\n${1+1}b`)"), vec![r"a\n2b"]);
        // A member tag binds `this`.
        assert_eq!(run_ok("let o={p:'P',f(s,...v){return this.p+':'+s.join('/')+v.join('')}}; console.log(o.f`x${10}y`)"), vec!["P:x/y10"]);
        // A closure capturing an outer var inside an interpolation.
        assert_eq!(run_ok("function t(s,...v){return v[0]} function mk(n){return ()=>t`${n*2}`} console.log(mk(21)())"), vec!["42"]);
    }

    #[test]
    fn typeof_operator() {
        // null is "object" (the historic quirk); arrays/objects "object";
        // functions/arrows "function"; primitives their type.
        assert_eq!(
            run_ok("let f=()=>1; console.log(typeof 1, typeof 1.5, typeof 'a', typeof true, typeof undefined, typeof null, typeof [], typeof {}, typeof f)"),
            vec!["number number string boolean undefined object object object function"],
        );
        // typeof sees through a captured (cell) variable to its value.
        assert_eq!(
            run_ok("let n=5; let g=()=>typeof n; console.log(g())"),
            vec!["number"],
        );
    }

    #[test]
    fn string_index_double_key_region() {
        // s[k] where k is a region-computed integral double (k = i*0+2) must
        // return the char, not undefined — the get_index Str arm coerces integral
        // doubles like the Array arm. JIT-on must equal JIT-off across the window.
        assert_jit_matches(
            "let s='ABCD'; let k=0; let c=0; for(let i=0;i<2000;i++){ k=i*0+2; if(s[k]==='C') c=c+1; } console.log(c)",
            &["2000"],
        );
    }

    #[test]
    fn array_setindex_sparse_grow_deopts() {
        // A sparse write past the end inside a hot loop deopts the (potentially
        // huge) resize to the interpreter rather than reallocating from native
        // code; the result still matches. a starts len 3; a[10]=i grows it to 11.
        assert_jit_matches(
            "let a=[1,2,3]; let n=0; for(let i=0;i<2000;i++){ a[10]=i; n=n+1; } console.log(a.length, n)",
            &["11 2000"],
        );
    }

    #[test]
    fn array_setindex_region_transform() {
        // In-place transform a[i] = a[i]*2 (GetIndex + SetIndex in one region).
        // JIT-on == JIT-off == 2*(0+..+999) = 999000.
        assert_jit_matches(
            "let a=[]; for(let i=0;i<1000;i++) a.push(i); for(let i=0;i<a.length;i++){ a[i]=a[i]*2; } let s=0; for(let i=0;i<a.length;i++) s+=a[i]; console.log(s)",
            &["999000"],
        );
    }

    #[test]
    fn array_setindex_build_and_grow() {
        // Build loop a[i]=i*i where every write GROWS the array (the helper grows
        // it, like the interpreter); plus a sparse write past the end with holes.
        assert_jit_matches(
            "let a=[]; for(let i=0;i<1000;i++){ a[i]=i*i; } let s=0; for(let i=0;i<1000;i++) s+=a[i]; console.log(a.length, s)",
            &["1000 332833500"],
        );
        assert_eq!(run_ok("let a=[]; a[5]=99; console.log(a.length, a[0], a[5])"), vec!["6 undefined 99"]);
    }

    #[test]
    fn array_length_assignment_in_region() {
        // `a.length = n` truncates a dense array; in a hot loop the result must
        // agree JIT-on == JIT-off == JS. Requires SROA to NOT scalar-promote the
        // special `length` property, and the write to deopt to the truncating
        // interpreter path rather than silently no-op.
        assert_jit_matches(
            "let a=[1,2,3,4,5]; let s=0; for(let i=0;i<20000;i++){ a.length=2; s+=a.length; } console.log(s)",
            &["40000"],
        );
    }

    #[test]
    fn array_length_clear_grow_invalid() {
        // arr.length = 0 clears (a very common idiom); larger extends with holes;
        // a non-integer / negative length throws RangeError.
        assert_eq!(run_ok("let a=[1,2,3]; a.length=0; console.log(a.length, a[0])"), vec!["0 undefined"]);
        assert_eq!(run_ok("let a=[1,2]; a.length=4; console.log(a.length, a[3], a[1])"), vec!["4 undefined 2"]);
        let out = run("let a=[1,2,3]; a.length=-1;").expect("compile");
        assert!(out.error.as_deref().unwrap_or("").contains("Invalid array length"));
    }

    #[test]
    fn array_double_and_oob_index() {
        // Integral double keys coerce (a[1.0]==a[1]); negative / fractional /
        // out-of-range keys are undefined — matching JS and the JIT helper.
        assert_eq!(
            run_ok("let a=[10,20,30]; console.log(a[1.0], a[2], a[5], a[-1], a[1.5])"),
            vec!["20 30 undefined undefined undefined"],
        );
    }

    #[test]
    fn recursive_callback_in_map_native() {
        // A self-recursive callback used in map exercises the native callback
        // fast path invoking a JIT'd self-recursive function (jit_self_call).
        // tri(n)=n+(n-1)+…; tri(3)=6, tri(4)=10, tri(5)=15.
        assert_eq!(
            run_ok("function tri(n){ return n<=0?0:n+tri(n-1); } console.log([3,4,5].map(tri).join(','))"),
            vec!["6,10,15"],
        );
    }

    #[test]
    fn int_function_modulo_jit() {
        // A hot function using `%` compiles via the whole-function JIT (idiv).
        // Negative dividends keep the dividend's sign (JS / interpreter
        // semantics); JIT-on must equal JIT-off and the expected string.
        assert_jit_matches(
            "function f(x){ return x%3; } let o=''; for(let i=-5;i<6;i++){ o += f(i)+','; } console.log(o)",
            &["-2,-1,0,-2,-1,0,1,2,0,1,2,"],
        );
    }

    #[test]
    fn modulo_zero_and_negone_bail() {
        // `% 0` is NaN and `% -1` is 0 — the JIT bails on both (div-by-zero, and
        // the INT_MIN/-1 #DE), so the interpreter produces them; JIT-on==JIT-off.
        assert_jit_matches(
            "function g(x,m){ return x%m; } let o=''; for(let i=0;i<10;i++){ o += g(i,0)+'|'+g(i,-1)+';'; } console.log(o)",
            &["NaN|0;NaN|0;NaN|0;NaN|0;NaN|0;NaN|0;NaN|0;NaN|0;NaN|0;NaN|0;"],
        );
    }

    #[test]
    fn string_bracket_length_matches_dot_length() {
        // s['length'] (computed member) must equal s.length and arr['length'] —
        // the get_index Str arm used to drop non-int keys to undefined.
        assert_eq!(
            run_ok("let s='hello'; let a=[1,2,3]; console.log(s['length'], s.length, a['length'])"),
            vec!["5 5 3"],
        );
    }
}
