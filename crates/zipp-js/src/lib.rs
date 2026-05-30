//! ZIPP dynamic JavaScript engine (Lane 3).
//!
//! A tree-walking interpreter that runs untyped JavaScript with real JS
//! semantics — the foundation of the "be a real JS engine" direction (see the
//! `js-engine-direction` memory). Pipeline: oxc parse → owned AST ([`lower`]) →
//! [`interp`] over a dynamic [`value::JsValue`].
//!
//! This is v0 (breadth-first correctness): operators with coercion, control
//! flow, functions/closures, objects, arrays, `throw`/`try`, template literals,
//! and a core stdlib (`console`, `Math`, `JSON`, `Object`, `Array`, `Number`,
//! `String`, array/string methods). Deferred to later tiers: prototypes/classes/
//! `new`, `for-in`/`for-of`, generators, `async`/`await`, modules, destructuring,
//! a bytecode tier + inline caches + a speculating JIT (to chase V8 on hot code).

mod ast;
mod builtins;
mod env;
mod interp;
mod json;
mod lower;
mod methods;
mod value;

use oxc_allocator::Allocator;
use oxc_parser::Parser;
use oxc_span::SourceType;

pub use interp::Interp;
pub use value::JsValue;

/// The result of running a program: the `console` output produced, plus an
/// optional uncaught-throw message (runtime errors don't discard prior output,
/// matching how a JS engine flushes stdout before reporting the error).
pub struct Outcome {
    pub output: Vec<String>,
    pub error: Option<String>,
}

/// Parse + run a JavaScript source string. `Err` is a *compile-time* failure
/// (parse or unsupported-syntax error); a runtime uncaught throw is reported via
/// [`Outcome::error`] alongside any output produced before it.
pub fn run(src: &str) -> Result<Outcome, String> {
    let allocator = Allocator::default();
    let ret = Parser::new(&allocator, src, SourceType::default()).parse();
    if !ret.errors.is_empty() {
        return Err(format!("SyntaxError: {}", ret.errors[0]));
    }
    let program = lower::lower_program(&ret.program)?;
    let interp = Interp::new();
    let error = match interp.run(&program) {
        Ok(()) => None,
        Err(thrown) => Some(format_thrown(&thrown)),
    };
    Ok(Outcome { output: interp.out.into_inner(), error })
}

fn format_thrown(v: &JsValue) -> String {
    if let JsValue::Object(o) = v {
        let b = o.borrow();
        if let Some(name) = b.props.get("name") {
            let msg = b.props.get("message").map(|m| m.to_js_string()).unwrap_or_default();
            return format!("Uncaught {}: {}", name.to_js_string(), msg);
        }
    }
    format!("Uncaught {}", v.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run `src` and return its joined console output (panicking on compile error
    /// or uncaught throw — tests assert success).
    fn out(src: &str) -> String {
        let o = run(src).expect("compile");
        assert!(o.error.is_none(), "unexpected throw: {:?}", o.error);
        o.output.join("\n")
    }

    #[test]
    fn arithmetic_and_coercion() {
        assert_eq!(out("console.log(1 + 2 * 3)"), "7");
        assert_eq!(out("console.log(10 / 4)"), "2.5");
        assert_eq!(out("console.log('a' + 1)"), "a1");
        assert_eq!(out("console.log(1 + '2' + 3)"), "123");
        assert_eq!(out("console.log(2 ** 10)"), "1024");
        assert_eq!(out("console.log(7 % 3)"), "1");
        assert_eq!(out("console.log('5' * 2)"), "10");
        assert_eq!(out("console.log(true + true)"), "2");
    }

    #[test]
    fn equality_and_logic() {
        assert_eq!(out("console.log(1 == '1')"), "true");
        assert_eq!(out("console.log(1 === '1')"), "false");
        assert_eq!(out("console.log(null == undefined)"), "true");
        assert_eq!(out("console.log(null === undefined)"), "false");
        assert_eq!(out("console.log(NaN === NaN)"), "false");
        assert_eq!(out("console.log(0 || 'x')"), "x");
        assert_eq!(out("console.log(1 && 2)"), "2");
        assert_eq!(out("console.log(null ?? 'd')"), "d");
        assert_eq!(out("console.log(0 ?? 'd')"), "0");
    }

    #[test]
    fn variables_and_control_flow() {
        assert_eq!(
            out("let s = 0; for (let i = 1; i <= 5; i++) { s += i; } console.log(s)"),
            "15"
        );
        assert_eq!(
            out("let i = 0, s = 0; while (i < 10) { if (i % 2 === 0) s += i; i++; } console.log(s)"),
            "20"
        );
        assert_eq!(
            out("let n = 3; let r = n > 2 ? 'big' : 'small'; console.log(r)"),
            "big"
        );
    }

    #[test]
    fn functions_and_closures() {
        assert_eq!(
            out("function add(a, b) { return a + b; } console.log(add(2, 3))"),
            "5"
        );
        assert_eq!(
            out("function fib(n){ return n < 2 ? n : fib(n-1) + fib(n-2); } console.log(fib(10))"),
            "55"
        );
        assert_eq!(
            out("function adder(n){ return (x) => x + n; } const a = adder(10); console.log(a(5))"),
            "15"
        );
        assert_eq!(
            out("function counter(){ let c = 0; return () => ++c; } const c = counter(); c(); c(); console.log(c())"),
            "3"
        );
    }

    #[test]
    fn objects_and_arrays() {
        assert_eq!(out("const o = { a: 1, b: 2 }; console.log(o.a + o['b'])"), "3");
        assert_eq!(out("const o = {}; o.x = 5; o.x += 2; console.log(o.x)"), "7");
        assert_eq!(out("const a = [1, 2, 3]; a.push(4); console.log(a.length)"), "4");
        assert_eq!(out("const a = [3, 1, 2]; console.log(a[0] + a[2])"), "5");
        assert_eq!(out("console.log(JSON.stringify({ a: 1, b: [2, 3] }))"), "{\"a\":1,\"b\":[2,3]}");
    }

    #[test]
    fn array_methods() {
        assert_eq!(
            out("console.log([1,2,3,4].map(x => x*2).filter(x => x > 4).reduce((a,b) => a+b, 0))"),
            "12"
        );
        assert_eq!(out("console.log([1,2,3].join('-'))"), "1-2-3");
        assert_eq!(out("console.log([3,1,2].sort((a,b) => a-b).join(','))"), "1,2,3");
        assert_eq!(out("console.log([1,2,3].includes(2))"), "true");
        assert_eq!(out("console.log(['a','b','c'].indexOf('b'))"), "1");
    }

    #[test]
    fn string_methods() {
        assert_eq!(out("console.log('hello'.toUpperCase())"), "HELLO");
        assert_eq!(out("console.log('a,b,c'.split(',').length)"), "3");
        assert_eq!(out("console.log('  hi  '.trim())"), "hi");
        assert_eq!(out("console.log('abc'.slice(1))"), "bc");
        assert_eq!(out("console.log('ab'.repeat(3))"), "ababab");
        assert_eq!(out("console.log(`sum is ${1 + 2}`)"), "sum is 3");
    }

    #[test]
    fn exceptions() {
        assert_eq!(
            out("try { throw new_error(); } catch (e) { console.log('caught ' + e); } function new_error(){ return 'boom'; }"),
            "caught boom"
        );
        assert_eq!(
            out("function f(){ try { return 'a'; } finally { console.log('fin'); } } console.log(f())"),
            "fin\na"
        );
        // an engine error (calling a non-function) is catchable
        assert_eq!(
            out("try { let x = 5; x(); } catch (e) { console.log(e.name); }"),
            "TypeError"
        );
    }

    #[test]
    fn builtins_math_json() {
        assert_eq!(out("console.log(Math.max(1, 9, 3))"), "9");
        assert_eq!(out("console.log(Math.floor(3.7))"), "3");
        assert_eq!(out("console.log(Object.keys({a:1, b:2}).join(','))"), "a,b");
        assert_eq!(out("console.log(JSON.parse('[1,2,3]')[1])"), "2");
        assert_eq!(out("console.log(JSON.parse('{\"x\": 42}').x)"), "42");
        assert_eq!(out("console.log(parseInt('0xff'))"), "255");
        assert_eq!(out("console.log(typeof 5, typeof 'a', typeof undefined, typeof (() => 1))"), "number string undefined function");
    }
}
