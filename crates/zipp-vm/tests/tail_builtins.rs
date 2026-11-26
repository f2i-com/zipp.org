//! Built-in global functions called in tail position.
//!
//! In strict code (a module's, a class body's, a `"use strict"` script's) a
//! `return f(x)` is a tail call, lowered as a frame-reuse prefix (`TailCall`)
//! and then the call. For a bare built-in (`return Boolean(x)`, `String(x)`,
//! `Number(x)`, `parseInt(s)`, ...) the call after the prefix was a plain
//! `Call`, not the guarded `GlobalFn` every other call of it is, so the
//! built-in was called the general way: `return Boolean(x)` cost 29 ns where
//! `const b = Boolean(x); return b` cost 12. The prefix is now followed by
//! the `GlobalFn`, and whole-function compiled code does `Boolean(x)` of the
//! boot `Boolean` itself (8 ns, as `!!x`).
//!
//! The program calls each built-in from tail and other positions with every
//! kind of value (the two forms must agree, and `Boolean` with `!!x`); with
//! objects whose conversions are observed and ones that throw; from class
//! bodies and arrows; with a parameter, a local and an upvalue of the same
//! name; and with the global rebound to other functions and back while the
//! sites are hot.
//!
//! The expected lines are Node 24's.

const PROGRAM: &str = r#"
"use strict";
const out = [];
const log = (...a) => out.push(a.join(" "));
const N = 3000;
function hot(fn) { let last; for (let i = 0; i < N; i++) last = fn(i); return last; }
function err(fn) { try { return fn(); } catch (e) { return e.constructor.name; } }
const sym = Symbol("s");
const values = [0, -0, 1, -1, NaN, 0.5, -0.5, Infinity, "", "0", "a", " ", null, undefined, true, false, {}, [], [0], 0n, 1n, -1n, 12345678901234567890n, sym, function () {}, () => 0, new Boolean(false), new Number(0), new String(""), Object.create(null), new Map(), new Set(), /x/, 2147483647, -2147483648, 4294967296, 1e-320];
// Boolean: tail, non-tail, in a condition, and against `!!x`.
function bTail(v) { return Boolean(v); }
function bNon(v) { const r = Boolean(v); return r; }
function bCond(v) { return Boolean(v) ? "t" : "f"; }
function bLogical(v) { return v !== 0 && Boolean(v); }
function bTernary(v) { return v === 1 ? Boolean(v) : Boolean(!v); }
function bSeq(v) { return (void 0, Boolean(v)); }
function bZero() { return Boolean(); }
function bTwo(v) { return Boolean(v, 1); }
log("boolean", hot((i) => { let s = ""; for (let k = 0; k < values.length; k++) { const v = values[k]; const a = bTail(v), b = bNon(v); if (a !== b || a !== !!v || typeof a !== "boolean" || bCond(v) !== (v ? "t" : "f") || bSeq(v) !== a || bTwo(v) !== a) s += "MISMATCH" + k; s += a ? "1" : "0"; } return s + bZero() + bLogical(0) + bLogical(2) + bTernary(1) + bTernary(0); }));
// String, Number, parseInt, parseFloat, isNaN, isFinite in tail position.
function sTail(v) { return String(v); }
function nTail(v) { return Number(v); }
function piTail(v, r) { return parseInt(v, r); }
function pfTail(v) { return parseFloat(v); }
function nanTail(v) { return isNaN(v); }
function finTail(v) { return isFinite(v); }
const texts = ["12", "0x1f", " 7.5e1x", "", "abc", "-0", "1e3", "Infinity", "101"];
log("string", hot((i) => values.map((v) => typeof v === "symbol" ? sTail(v) : err(() => sTail(v))).join("|")));
log("number", hot((i) => values.map((v) => err(() => String(nTail(v)))).join("|")));
log("parse", hot((i) => texts.map((t) => piTail(t) + "," + piTail(t, 2) + "," + piTail(t, 16) + "," + pfTail(t) + "," + nanTail(t) + "," + finTail(t)).join("|")));
log("int args", hot((i) => [sTail(i), nTail(i), piTail(i), pfTail(i + 0.5), nanTail(i), finTail(i / 0), sTail(i * 1.5), sTail(-i)].join(",")));
// An object argument's conversions run, in order, and their errors propagate.
const trace = [];
const loud = { valueOf() { trace.push("valueOf"); return 42; }, toString() { trace.push("toString"); return "loud"; } };
const bad = { valueOf() { throw new RangeError("bad valueOf"); }, toString() { throw new RangeError("bad toString"); } };
log("coerce", hot(() => { trace.length = 0; const r = [bTail(loud), sTail(loud), nTail(loud), piTail(loud), nanTail(loud)].join(","); return r + ":" + trace.join(">"); }), err(() => hot(() => sTail(bad))), err(() => hot(() => nTail(bad))), hot(() => bTail(bad)), err(() => hot(() => nTail(sym))), err(() => hot(() => nTail(1n))));
// Class bodies and arrows are strict code too.
class Box { constructor(v) { this.v = v; } get truthy() { return Boolean(this.v); } text() { return String(this.v); } static of(v) { return Number(v); } }
const arrow = (v) => Boolean(v);
log("class", hot((i) => { const b = new Box(i & 1); return [b.truthy, b.text(), Box.of("7"), arrow(i & 2), [0, 1, "", "x"].filter(Boolean).length, [1, 2].map(String).join(""), ["1", "2"].map(Number).join("")].join(","); }));
// A local, a parameter and an upvalue of the same name are not the global.
function shadowParam(Boolean, v) { return Boolean(v); }
function shadowLocal(v) { const String = (x) => "local:" + x; return String(v); }
function shadowUpvalue() { const Number = (x) => x + 1000; return (v) => Number(v); }
const viaUp = shadowUpvalue();
log("shadows", hot((i) => [shadowParam((x) => "param:" + x, i), shadowLocal(i), viaUp(i)].join(",")));
// The global rebound while the sites are hot: every site calls the new value, and back.
const realBoolean = Boolean, realString = String, realNumber = Number, realParseInt = parseInt;
log("before", [bTail(0), bNon("x"), sTail(1), nTail("2"), piTail("11", 2)].join(","));
globalThis.Boolean = function (v) { return "mine:" + v; };
globalThis.String = (v) => "str:" + v;
globalThis.Number = function (v) { return this === undefined ? "strict-this" : "sloppy-this"; };
globalThis.parseInt = realString;
log("rebound", hot((i) => [bTail(0), bNon("x"), bCond(0), sTail(1), nTail("2"), piTail("11", 2)].join(",")));
globalThis.Boolean = realBoolean; globalThis.String = realString; globalThis.Number = realNumber; globalThis.parseInt = realParseInt;
log("restored", hot((i) => [bTail(0), bNon("x"), bCond(0), sTail(1), nTail("2"), piTail("11", 2)].join(",")));
// A name rebound to a plain function still makes a proper tail call: no stack growth.
globalThis.Boolean = function down(n) { return n === 0 ? "bottom" : Boolean(n - 1); };
function start(n) { return Boolean(n); }
log("tail depth", start(400));
globalThis.Boolean = realBoolean;
log("after", bTail(1), bTail(""), bZero());
console.log(out.join("\n"));
"#;

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output.iter().flat_map(|l| l.lines().map(str::to_string)).collect()
}

#[test]
fn tail_builtins_answers_as_node() {
    assert_eq!(
        run_ok(PROGRAM),
        [
            "boolean 0011011101110010111011111111111111111falsefalsetruetruetrue",
            "string 0|0|1|-1|NaN|0.5|-0.5|Infinity||0|a| |null|undefined|true|false|[object Object]||0|0|1|-1|12345678901234567890|Symbol(s)|function () {}|() => 0|false|0||TypeError|[object Map]|[object Set]|/x/|2147483647|-2147483648|4294967296|1e-320",
            "number 0|0|1|-1|NaN|0.5|-0.5|Infinity|0|0|NaN|0|0|NaN|1|0|NaN|0|0|0|1|-1|12345678901234567000|TypeError|NaN|NaN|0|0|0|TypeError|NaN|NaN|NaN|2147483647|-2147483648|4294967296|1e-320",
            "parse 12,1,18,12,false,true|31,0,31,0,false,true|7,NaN,7,75,true,false|NaN,NaN,NaN,NaN,false,true|NaN,NaN,2748,NaN,true,false|0,0,0,0,false,true|1,1,483,1000,false,true|NaN,NaN,NaN,Infinity,false,false|101,5,257,101,false,true",
            "int args 2999,2999,2999,2999.5,false,false,4498.5,-2999",
            "coerce true,loud,42,NaN,false:toString>valueOf>toString>valueOf RangeError RangeError true TypeError 1",
            "class true,1,7,true,2,12,12",
            "shadows param:2999,local:2999,3999",
            "before false,true,1,2,3",
            "rebound mine:0,mine:x,t,str:1,strict-this,11",
            "restored false,true,f,1,2,3",
            "tail depth bottom",
            "after true false false",
        ]
    );
}

/// The same answers with the JIT compiling at once, with a collection at
/// every safe point, and with no native code.
#[test]
fn tail_builtins_all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    for (name, key, value) in [
        ("eager", "ZIPP_JIT_THRESHOLD", "1"),
        ("stress", "ZIPP_GC_STRESS", "1"),
        ("nojit", "ZIPP_NOJIT", "1"),
    ] {
        let out = std::process::Command::new(&exe)
            .arg("tail_builtins_answers")
            .arg("--nocapture")
            .env(key, value)
            .output()
            .expect("spawn mode child");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{name} mode failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !stdout.contains("running 0 tests"),
            "the filter matched nothing in {name} mode"
        );
    }
}

/// A name rebound to a plain function is still a proper tail call: the
/// frame-reuse prefix takes it before the built-in's opcode is reached, so a
/// chain 300,000 deep runs in constant stack (Node, which has no proper tail
/// calls, cannot run this one).
#[test]
fn a_rebound_builtin_name_is_still_a_proper_tail_call() {
    const DEEP: &str = r#""use strict";
const realBoolean = Boolean, realString = String;
globalThis.Boolean = function down(n) { return n === 0 ? "bottom" : Boolean(n - 1); };
function start(n) { return Boolean(n); }
const a = start(300000);
globalThis.Boolean = realBoolean;
globalThis.String = function count(n, acc) { return n === 0 ? acc : String(n - 1, acc + 1); };
function startS(n) { return String(n, 0); }
const b = startS(300000);
globalThis.String = realString;
console.log(a, b, Boolean(0), String(7));
"#;
    assert_eq!(run_ok(DEEP), ["bottom 300000 false 7"]);
}
