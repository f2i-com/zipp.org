//! `new` in compiled code.
//!
//! A memory region or Tier C body now compiles `New` as a call to
//! `jit_new_ic`, which constructs exactly as the interpreter's `New` does;
//! before, a loop that constructed was not compiled at all. This program runs
//! each shape thousands of times inside functions, so the loops compile, and
//! checks: plain constructors with prototype methods, every kind of return
//! override, a throw mid-loop, a site that sees constructor functions,
//! classes and builtins in turn, `new.target` through a subclass, the
//! non-constructors that must throw a TypeError, a Proxy construct trap and a
//! bound constructor, side effects, `arguments`, and instances that survive
//! collections.
//!
//! The expected lines are Node 24's output for the same program.

use zipp_vm::embed;

fn run(source: &str) -> Vec<String> {
    let mut state = embed::compile_script(source).expect("source compiles");
    #[cfg(feature = "safe-sandbox")]
    state.set_limits(4_000_000_000, None);
    state.run_init().expect("source runs");
    state.take_output()
}

const PROGRAM: &str = r####"// Differential probe: `new` in hot compiled loops, every kind of constructor.
var out = [];
function log(label, v) { out.push(label + ": " + v); }

function P(x) { this.x = x; this.y = x * 2; }
P.prototype.sum = function () { return this.x + this.y; };
function loopPlain(n) { var s = 0; for (var i = 0; i < n; i++) { var p = new P(i); s = (s + p.sum()) | 0; } return s; }
log("plain", loopPlain(5000) + " " + loopPlain(10));

function R(kind) { this.k = kind; if (kind === 1) return { replaced: true }; if (kind === 2) return 42; if (kind === 3) return [7]; }
function loopReturn(n) { var c = [0, 0, 0, 0]; for (var i = 0; i < n; i++) { var r = new R(i % 4); c[i % 4] += r.replaced ? 1 : r.k === i % 4 ? 2 : Array.isArray(r) ? 3 : 4; } return c.join(","); }
log("return override", loopReturn(4000));

function T(i) { if (i === 3333) throw new RangeError("at " + i); this.i = i; }
function loopThrow(n) { var made = 0; try { for (var i = 0; i < n; i++) { new T(i); made++; } } catch (e) { return made + " " + e.name + " " + e.message; } return made; }
log("throw", loopThrow(5000) + " | " + loopThrow(100));

var ctors = [P, function Q(x) { this.q = x; }, class K { constructor(x) { this.k = x; } }, Array, Date, Object, Number];
function loopMixed(n) { var names = []; for (var i = 0; i < n; i++) { var o = new ctors[i % ctors.length](i); if (i < 12) names.push(Object.prototype.toString.call(o).slice(8, -1) + (o.x !== undefined ? o.x : o.q !== undefined ? o.q : o.k !== undefined ? o.k : "")); } return names.join(","); }
log("mixed", loopMixed(3000));

function NT() { this.nt = new.target === NT ? "NT" : new.target ? new.target.name : "none"; }
class SubNT extends NT {}
function loopNT(n) { var r = ""; for (var i = 0; i < n; i++) { var o = i % 2 ? new NT() : new SubNT(); if (i < 4) r += o.nt + (o instanceof SubNT) + ","; } return r; }
log("new.target", loopNT(4000));

var notCtors = [function () {}.bind(null), () => 1, Math.max, Symbol, 5, { a: 1 }, function* g() {}];
function loopBad(n) { var r = []; for (var i = 0; i < n; i++) { try { var o = new notCtors[i % notCtors.length](); if (i < 7) r.push("ok:" + typeof o); } catch (e) { if (i < 7) r.push(e.name); } } return r.join(","); }
log("non-constructors", loopBad(3500));

var traps = 0;
var Prox = new Proxy(P, { construct(t, args, nt) { traps++; return { proxied: args[0] }; } });
var Bound = P.bind(null, 100);
function loopExotic(n) { var s = 0; for (var i = 0; i < n; i++) { s += new Prox(i).proxied + new Bound().x; } return s; }
log("proxy bound", loopExotic(2000) + " " + traps);

function Counter() { Counter.made = (Counter.made || 0) + 1; this.id = Counter.made; }
function loopSide(n) { var last; for (var i = 0; i < n; i++) last = new Counter(); return last.id + " " + Counter.made; }
log("side effects", loopSide(3000));

function Args() { this.n = arguments.length; this.a = Array.prototype.slice.call(arguments).join("-"); }
function loopArgs(n) { var r; for (var i = 0; i < n; i++) r = new Args(i, i + 1, "z"); return r.n + " " + r.a + " " + new Args().n; }
log("arguments", loopArgs(3000));

function G(x) { this.x = x; }
function loopGc(n) { var keep = []; for (var i = 0; i < n; i++) { var o = new G([i, { deep: i }]); if (i % 1000 === 0) keep.push(o); } return keep.length + " " + keep[keep.length - 1].x[1].deep; }
log("gc", loopGc(30000));

console.log(out.join("\n"));
"####;

#[test]
fn new_in_compiled_loops_matches_node() {
    let expected: &[&str] = &[
        r####"plain: 37492500 135"####,
        r####"return override: 2000,1000,2000,3000"####,
        r####"throw: 3333 RangeError at 3333 | 100"####,
        r####"mixed: Object0,Object1,Object2,Array,Date,Number,Number,Object7,Object8,Object9,Array,Date"####,
        r####"new.target: SubNTtrue,NTfalse,SubNTtrue,NTfalse,"####,
        r####"non-constructors: ok:object,TypeError,TypeError,TypeError,TypeError,TypeError,TypeError"####,
        r####"proxy bound: 2199000 2000"####,
        r####"side effects: 3000 3000"####,
        r####"arguments: 3 2999-3000-z 0"####,
        r####"gc: 30 29000"####,
    ];
    let out = run(PROGRAM);
    let got: Vec<&str> = out.iter().flat_map(|chunk| chunk.lines()).collect();
    assert_eq!(got.len(), expected.len(), "{got:#?}");
    for (got, want) in got.iter().zip(expected) {
        assert_eq!(got, want);
    }
}
