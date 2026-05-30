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
        // `name` is typically inherited from the error prototype; `message` is an
        // own property. Walk the chain for either.
        let lookup = |key: &str| -> Option<String> {
            let mut cur = Some(o.clone());
            while let Some(c) = cur {
                if let Some(val) = c.borrow().props.get(key) {
                    return Some(val.to_js_string());
                }
                cur = c.borrow().proto.clone();
            }
            None
        };
        if let Some(name) = lookup("name") {
            let msg = lookup("message").unwrap_or_default();
            if msg.is_empty() {
                return format!("Uncaught {name}");
            }
            return format!("Uncaught {name}: {msg}");
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
    fn number_formatting() {
        // ECMAScript Number::toString — exponential thresholds + no i64 overflow.
        assert_eq!(out("console.log(1e21)"), "1e+21");
        assert_eq!(out("console.log(1e-7)"), "1e-7");
        assert_eq!(out("console.log(1e-6)"), "0.000001");
        assert_eq!(out("console.log(1e30)"), "1e+30");
        assert_eq!(out("console.log(1.5e-10)"), "1.5e-10");
        assert_eq!(out("console.log(1e100)"), "1e+100");
        // large integers must NOT saturate to i64::MAX
        assert_eq!(out("console.log(100000000000000000000)"), "100000000000000000000");
        assert_eq!(out("console.log(1234567890123456789012345)"), "1.2345678901234568e+24");
        // ordinary values
        assert_eq!(out("console.log(255.5)"), "255.5");
        assert_eq!(out("console.log(0.1)"), "0.1");
        assert_eq!(out("console.log(1000000)"), "1000000");
        assert_eq!(out("console.log(-1234.5678)"), "-1234.5678");
        // console.log shows -0 with its sign, but String(-0)/toString give "0".
        assert_eq!(out("console.log(-0)"), "-0");
        assert_eq!(out("console.log(String(-0), `${-0}`, (-0).toString())"), "0 0 0");
        assert_eq!(out("console.log(0)"), "0");
        assert_eq!(out("console.log(-0 === 0)"), "true");
        assert_eq!(out("console.log(123.456)"), "123.456");
        assert_eq!(out("console.log(-1e-21)"), "-1e-21");
    }

    #[test]
    fn math_signed_zero() {
        // round of a negative in [-0.5, 0) is -0; round ties go toward +Infinity.
        assert_eq!(out("console.log(Math.round(-0.2), Math.round(-0.5))"), "-0 -0");
        assert_eq!(out("console.log(Math.round(2.5), Math.round(0.5), Math.round(-0.6))"), "3 1 -1");
        // max prefers +0 over -0; min prefers -0 over +0.
        assert_eq!(out("console.log(Math.max(-0, 0), Math.max(0, -0))"), "0 0");
        assert_eq!(out("console.log(Math.min(-0, 0), Math.min(0, -0))"), "-0 -0");
        // sign / floor / ceil preserve -0.
        assert_eq!(out("console.log(Math.sign(-0), Math.floor(-0), Math.ceil(-0.5))"), "-0 -0 -0");
    }

    #[test]
    fn number_constants() {
        assert_eq!(out("console.log(Number.MAX_VALUE)"), "1.7976931348623157e+308");
        assert_eq!(out("console.log(Number.MIN_VALUE)"), "5e-324");
        assert_eq!(out("console.log(Number.MAX_VALUE > 1e308, Number.MIN_VALUE > 0)"), "true true");
    }

    #[test]
    fn to_number_coercion() {
        // ToNumber(object) via ToPrimitive (string form), and radix-prefixed /
        // Infinity / invalid string forms.
        assert_eq!(out("console.log(Number([]))"), "0");
        assert_eq!(out("console.log([] - 1)"), "-1");
        assert_eq!(out("console.log(+[])"), "0");
        assert_eq!(out("console.log(Number([5]), Number([1,2]), Number({}))"), "5 NaN NaN");
        assert_eq!(out("console.log(Number('0b101'), Number('0o17'), Number('0xff'))"), "5 15 255");
        assert_eq!(out("console.log(Number(''), Number('  42  '), Number('3.14'))"), "0 42 3.14");
        assert_eq!(out("console.log(Number('Infinity'), Number('-Infinity'))"), "Infinity -Infinity");
        // Rust's f64 parser accepts these; JS does not.
        assert_eq!(out("console.log(Number('inf'), Number('nan'), Number('abc'))"), "NaN NaN NaN");
        assert_eq!(out("console.log([10] - 5, +'  7  ')"), "5 7");
    }

    #[test]
    fn flatmap_and_reduceright() {
        // flatMap == map then flatten one level (and only one level)
        assert_eq!(out("console.log(JSON.stringify([1,2,3].flatMap(x => [x, x*2])))"), "[1,2,2,4,3,6]");
        assert_eq!(out("console.log(JSON.stringify([1,2,3].flatMap(x => x*2)))"), "[2,4,6]");
        assert_eq!(out("console.log(JSON.stringify([1,2,3].flatMap(x => [[x]])))"), "[[1],[2],[3]]");
        // reduceRight walks right-to-left
        assert_eq!(out("console.log([1,2,3,4].reduceRight((a,b) => a + '-' + b))"), "4-3-2-1");
        assert_eq!(out("console.log([1,2,3,4].reduceRight((a,b) => a + b, 100))"), "110");
        assert_eq!(out("console.log([5].reduceRight((a,b) => a + b))"), "5");
        assert_eq!(out("console.log([1,2,3].reduceRight((a,b,i) => a + ':' + i + b, 'start'))"), "start:23:12:01");
    }

    #[test]
    fn array_search_from_index() {
        assert_eq!(out("var a=[1,2,3,2,1]; console.log(a.indexOf(2), a.indexOf(2,2), a.indexOf(2,-2))"), "1 3 3");
        assert_eq!(out("var a=[1,2,3,2,1]; console.log(a.indexOf(2,100), a.indexOf(2,-100), a.indexOf(1,3))"), "-1 1 4");
        assert_eq!(out("var a=[1,2,3,2,1]; console.log(a.lastIndexOf(2), a.lastIndexOf(2,2), a.lastIndexOf(1,0))"), "3 1 0");
        assert_eq!(out("var a=[1,2,3,2,1]; console.log(a.lastIndexOf(1,-100), a.lastIndexOf(2,100))"), "-1 3");
        assert_eq!(out("var a=[1,2,3,2,1]; console.log(a.includes(2,2), a.includes(2,4), a.includes(1,-1))"), "true false true");
        // includes uses SameValueZero (NaN matches); indexOf uses === (does not)
        assert_eq!(out("console.log([NaN].includes(NaN), [NaN].indexOf(NaN), [0].includes(-0))"), "true -1 true");
        // fractional fromIndex truncates; empty arrays
        assert_eq!(out("var a=[1,2,3,2,1]; console.log(a.indexOf(2,1.9), [].indexOf(1), [].includes(1))"), "1 -1 false");
    }

    #[test]
    fn array_join_to_string() {
        assert_eq!(out("var o={toString(){return 'X'}}; console.log([o,o].join('|'))"), "X|X");
        assert_eq!(out("var p={v:7,toString(){return '<'+this.v+'>'}}; console.log([p,5,p].join(' '))"), "<7> 5 <7>");
        assert_eq!(out("console.log([{a:1},{b:2}].join(','))"), "[object Object],[object Object]");
        assert_eq!(out("console.log([1,null,2,undefined,3].join(','))"), "1,,2,,3");
        assert_eq!(out("console.log([[1,2],[3,4]].join(';'), [1,[2,[3,4]]].join('-'))"), "1,2;3,4 1-2,3,4");
    }

    #[test]
    fn object_is() {
        // SameValue: NaN==NaN, +0!=-0, otherwise like ===
        assert_eq!(out("console.log(Object.is(NaN, NaN), Object.is(0, -0), Object.is(-0, -0))"), "true false true");
        assert_eq!(out("console.log(Object.is('a','a'), Object.is(1,1), Object.is(1,'1'))"), "true true false");
        assert_eq!(out("var o={}; console.log(Object.is(o,o), Object.is({},{}))"), "true false");
        assert_eq!(out("console.log(Object.is(null,null), Object.is(null,undefined), Object.is())"), "true false true");
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
    fn user_tostring_in_coercion() {
        // String(obj), template literals, and `+` use a user toString().
        let p = "class P{constructor(x){this.x=x;} toString(){return 'P'+this.x;}}";
        assert_eq!(out(&format!("{p} console.log(String(new P(5)))")), "P5");
        assert_eq!(out(&format!("{p} const o=new P(7); console.log(`v=${{o}}`)")), "v=P7");
        assert_eq!(out(&format!("{p} console.log('' + new P(9))")), "P9");
        assert_eq!(out(&format!("{p} console.log(new P(1) + '!')")), "P1!");
        // object-literal toString
        assert_eq!(out("const o={toString(){return 'C';}}; console.log(String(o), `${o}`, 'x'+o)"), "C C xC");
        // Error coerces via its prototype toString → "Error: msg"
        assert_eq!(out("console.log(String(new Error('oops')))"), "Error: oops");
        // plain object + primitives keep default coercion
        assert_eq!(out("console.log(String({}), String(42), String(null), String([1,2]))"), "[object Object] 42 null 1,2");
    }

    #[test]
    fn error_constructors() {
        assert_eq!(out("const e=new Error('boom'); console.log(e.message, e.name)"), "boom Error");
        assert_eq!(out("console.log(new Error('x') instanceof Error)"), "true");
        assert_eq!(out("console.log(new TypeError('t').name, new TypeError('t') instanceof Error)"), "TypeError true");
        assert_eq!(out("console.log(new RangeError('r') instanceof Error, new RangeError('r') instanceof RangeError)"), "true true");
        assert_eq!(out("console.log(new Error('m').toString())"), "Error: m");
        assert_eq!(out("console.log(new TypeError('z').toString())"), "TypeError: z");
        assert_eq!(out("console.log(new Error().message === '', new Error().name)"), "true Error");
        // thrown user error is catchable + carries message/instanceof
        assert_eq!(
            out("try { throw new Error('thrown'); } catch (e) { console.log(e.message, e instanceof Error); }"),
            "thrown true"
        );
        // engine-thrown error shares the prototype chain
        assert_eq!(
            out("try { null.x; } catch (e) { console.log(e.name, e instanceof TypeError, e instanceof Error); }"),
            "TypeError true true"
        );
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
