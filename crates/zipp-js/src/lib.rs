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
            "14" // [2,4,6,8] -> filter >4 -> [6,8] -> 6+8 = 14
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

    #[test]
    fn getters_setters() {
        // object-literal getter
        assert_eq!(out("const o={a:2,get d(){return this.a*2;}}; console.log(o.d)"), "4");
        // object-literal setter
        assert_eq!(
            out("const o={_x:0,get x(){return this._x;},set x(v){this._x=v+1;}}; o.x=5; console.log(o.x)"),
            "6"
        );
        // class instance getter + setter
        assert_eq!(
            out("class T{constructor(c){this._c=c;} get f(){return this._c*9/5+32;} set f(v){this._c=(v-32)*5/9;}} const t=new T(0); console.log(t.f); t.f=212; console.log(t._c)"),
            "32\n100"
        );
        // class getter using another property
        assert_eq!(
            out("class C{constructor(r){this.r=r;} get area(){return this.r*this.r;}} console.log(new C(4).area)"),
            "16"
        );
        // setter-only: reading yields undefined
        assert_eq!(out("const o={set w(v){this._w=v;}}; o.w=3; console.log(o._w, o.w)"), "3 undefined");
    }

    #[test]
    fn object_spread() {
        assert_eq!(out("const a={x:1,y:2}; console.log(JSON.stringify({...a,z:3}))"), "{\"x\":1,\"y\":2,\"z\":3}");
        // later keys win over spread
        assert_eq!(out("const a={x:1}; console.log(JSON.stringify({...a,x:99}))"), "{\"x\":99}");
        // spread overrides an earlier literal key
        assert_eq!(out("const a={x:7}; console.log(JSON.stringify({x:1,...a}))"), "{\"x\":7}");
        // null/undefined sources are skipped
        assert_eq!(out("console.log(JSON.stringify({...null,...undefined,k:1}))"), "{\"k\":1}");
        // shallow clone is independent
        assert_eq!(out("const o={p:1}; const c={...o}; c.p=2; console.log(o.p,c.p)"), "1 2");
    }

    #[test]
    fn destructuring() {
        assert_eq!(out("const {a,b}={a:1,b:2,c:3}; console.log(a,b)"), "1 2");
        assert_eq!(out("const {a:x,c:y}={a:1,b:2,c:3}; console.log(x,y)"), "1 3");
        assert_eq!(out("const {m=99,a=5}={a:1}; console.log(m,a)"), "99 1");
        assert_eq!(out("const {n:{x,y}}={n:{x:10,y:20}}; console.log(x,y)"), "10 20");
        assert_eq!(out("const [p,q]=[10,20,30]; console.log(p,q)"), "10 20");
        assert_eq!(out("const [,,z]=[10,20,30]; console.log(z)"), "30");
        assert_eq!(out("const [h,...t]=[1,2,3,4]; console.log(h, t.join(','))"), "1 2,3,4");
        assert_eq!(out("const [a=1,,c=100]=[7,8]; console.log(a,c)"), "7 100");
        assert_eq!(out("const [[a,b],[c]]=[[1,2],[3,4]]; console.log(a,b,c)"), "1 2 3");
    }

    #[test]
    fn optional_chaining() {
        assert_eq!(out("const o={a:{b:{c:42}}}; console.log(o?.a?.b?.c)"), "42");
        assert_eq!(out("const o={a:1}; console.log(o?.x?.y?.z)"), "undefined");
        assert_eq!(out("const o={}; console.log(o?.missing?.deep ?? 'dflt')"), "dflt");
        assert_eq!(out("const o={list:[10,20,30]}; console.log(o?.list?.[1], o?.gone?.[5])"), "20 undefined");
        assert_eq!(out("const o={fn:()=>'hi'}; console.log(o?.fn?.(), o?.nofn?.())"), "hi undefined");
        assert_eq!(out("const u=null; console.log(u?.a?.b, u?.x ?? 'safe')"), "undefined safe");
        assert_eq!(
            out("function g(x){ return x?.value?.toUpperCase(); } console.log(g({value:'hi'}), g(null), g({}))"),
            "HI undefined undefined"
        );
    }

    #[test]
    fn classes_and_inheritance() {
        assert_eq!(
            out("class P { constructor(x,y){ this.x=x; this.y=y; } sum(){ return this.x+this.y; } static make(){ return new P(1,2); } } const p = new P(3,4); console.log(p.sum(), P.make().sum(), p instanceof P)"),
            "7 3 true"
        );
        assert_eq!(
            out("class C { n=0; inc(){ this.n++; return this; } } const c = new C(); c.inc().inc(); console.log(c.n)"),
            "2"
        );
        assert_eq!(
            out("class A { constructor(n){ this.n=n; } who(){ return 'A'+this.n; } } class B extends A { who(){ return super.who()+'/B'; } } const b = new B(5); console.log(b.who(), b instanceof A, b instanceof B)"),
            "A5/B true true"
        );
        assert_eq!(out("const o={a:1}; console.log('a' in o, 'b' in o)"), "true false");
    }

    #[test]
    fn spread_and_params() {
        assert_eq!(out("function s(...xs){ return xs.reduce((a,b)=>a+b,0); } console.log(s(1,2,3,4))"), "10");
        assert_eq!(out("const a=[1,2], b=[3,4]; console.log([...a,...b,5].join(','))"), "1,2,3,4,5");
        assert_eq!(out("function f(x,y,z){ return x+y+z; } console.log(f(...[1,2,3]))"), "6");
        assert_eq!(out("console.log(Math.max(...[3,7,2]))"), "7");
        assert_eq!(out("function g(a,b=10){ return a+b; } console.log(g(5), g(5,1))"), "15 6");
        assert_eq!(out("const m=(a,b=2)=>a*b; console.log(m(5), m(5,3))"), "10 15");
    }

    #[test]
    fn loops_switch_logical_assign() {
        assert_eq!(out("let s=0; for (const x of [1,2,3,4]) s+=x; console.log(s)"), "10");
        assert_eq!(out("let r=''; for (const c of 'abc') r+=c.toUpperCase(); console.log(r)"), "ABC");
        assert_eq!(out("const o={a:1,b:2}; let k=[]; for (const x in o) k.push(x); console.log(k.join(','))"), "a,b");
        assert_eq!(out("function g(n){ switch(n){ case 1: return 'one'; case 2: return 'two'; default: return '?'; } } console.log(g(1),g(2),g(9))"), "one two ?");
        assert_eq!(out("function d(n){ switch(n){ case 0: case 6: return 'wknd'; default: return 'wk'; } } console.log(d(6),d(3))"), "wknd wk");
        assert_eq!(out("let a=0; a||=5; let b=2; b||=9; console.log(a,b)"), "5 2");
        assert_eq!(out("let x; x??='d'; let y=0; y??='e'; console.log(x,y)"), "d 0");
        assert_eq!(out("let p=1; p&&=7; let q=0; q&&=7; console.log(p,q)"), "7 0");
    }
}
