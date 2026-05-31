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
    fn known_limitation_per_iteration_let_in_for() {
        // KNOWN GAP vs node: a `let` loop variable captured inside a for-loop
        // body should produce a FRESH binding per iteration (node → "0 1 2").
        // zipp-vm v1 shares one cell across iterations (→ "3 3 3", i.e. `var`
        // semantics). Documented here so the divergence is explicit and tracked;
        // per-iteration loop bindings are a future refinement.
        let out = run("function mk(){ let xs=[]; for(let i=0;i<3;i++){ xs.push(()=>i) } return xs } let f=mk(); console.log(f[0](), f[1](), f[2]())")
            .expect("compile");
        assert_eq!(out.output, vec!["3 3 3"]); // NOT node's "0 1 2" — see comment
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
