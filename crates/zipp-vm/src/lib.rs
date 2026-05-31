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
}
