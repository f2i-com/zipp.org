//! The lite loop (`vm/lite.rs`): simple functions run in a second, small
//! interpreter loop on the real frames, handing back to the main loop on
//! anything else. Every case here crosses that boundary or must stay exact
//! inside it: recursion, a callee replaced mid-run, closures, natives and
//! other ineligible callees and callers, throws in and through it, strings,
//! doubles, BigInts and objects through its arithmetic and compares, globals
//! uninitialized, undeclared or behind getters, generators driving it, and
//! the recursion limit. The expected lines are Node 24's.

const SOURCE: &str = r#"
var out = [];
var log = function () { out.push(Array.prototype.join.call(arguments, " ")); };
function catchName(f) { try { return f(); } catch (e) { return e.constructor.name; } }

function fib(n) { return n < 2 ? n : fib(n - 1) + fib(n - 2); }
log("fib", fib(18), fib(1), fib(0), fib(2.5), fib("7"), fib(-1));
log("fib-big", catchName(function () { return fib(3n); }));

// The global callee replaced while a recursion is running.
function countdown(n) { if (n === 0) { countdown = function () { return "replaced"; }; return 0; } return 1 + countdown(n - 1); }
log("replace", countdown(3), countdown(3));

// A lite function calling a native (Math.max) and an ineligible callee.
function useNative(a, b) { return Math.max(a, b) + 1; }
function ineligible(o) { return o.x + 1; }
function callsIneligible(n) { return n < 1 ? 0 : ineligible({ x: n }) + callsIneligible(n - 1); }
log("native", useNative(3, 9), callsIneligible(4));

// Returning into an ineligible caller.
var obj = { run: function (n) { return fib(n) + this.k; }, k: 100 };
log("caller", obj.run(10));

// Closures sharing one function.
function mk(k) { return function (x) { return x < 1 ? k : x + k; }; }
var a1 = mk(1), a2 = mk(10);
log("closures", a1(0), a1(5), a2(0), a2(5));

// Throws inside: caught outside, and through finally.
function boom(n) { if (n === 0) { null.x; } return 1 + boom(n - 1); }
log("throw", catchName(function () { return boom(5); }));
var fin = [];
function guarded(n) { try { return boom(n); } finally { fin.push("f" + n); } }
log("finally", catchName(function () { return guarded(3); }), fin.join(","));
function addv(a, b) { return a + b; }
log("valueOf-throw", catchName(function () { return addv({ valueOf: function () { throw new RangeError("v"); } }, 1); }));

// Arithmetic and compares on every kind of operand.
function sum3(a, b, c) { return a + b + c; }
function lt(a, b) { return a < b ? "lt" : "ge"; }
var vo = 0;
log("add", sum3(1, 2, 3), sum3(2147483647, 1, 0), sum3(0.5, 0.25, 1), sum3("a", 1, 2), sum3(1, 2, "c"), sum3(1n, 2n, 3n),
  sum3({ valueOf: function () { vo++; return 5; } }, 1, 1), vo, sum3(null, undefined, 1));
log("cmp", lt(1, 2), lt(2, 1), lt(1.5, 1.6), lt("a", "b"), lt("10", "9"), lt(1n, 2), lt(NaN, 1), lt(undefined, 1), lt(null, 1));

// Globals: uninitialized, undeclared, behind a getter.
function readLate() { return LATE + 1; }
log("tdz", catchName(readLate));
let LATE = 41;
log("late", readLate());
function readNope() { return NOPE + 1; }
log("undeclared", catchName(readNope));
Object.defineProperty(globalThis, "GG", { get: function () { return 7; }, configurable: true });
function readGetter() { return GG + 1; }
log("getter", readGetter(), readGetter());

// A loop with a counter step and bound, the lite set's jumps.
function loopSum(n) { var s = 0; for (var i = 0; i < n; i++) { s = s + i; } return s; }
function loopLe(n) { var s = 0; for (var i = 0; i <= n; i++) { s = s + 1; } return s; }
log("loops", loopSum(100), loopSum(0), loopLe(5), loopSum(2.5));

// Loops over properties and elements: own and inherited reads, one shape
// and many, a getter appearing mid-loop, writes, holes, out-of-range and
// negative indices, typed arrays (BigInt ones too), sparse arrays.
function readAll(objs, n) { var s = 0, k = 0; for (var i = 0; i < 40; i++) { s = (s + objs[k].p) | 0; k++; if (k === n) k = 0; } return s; }
var same = [], diff = [], proto = { p: 7 }, inh = [];
for (var q = 0; q < 9; q++) { var o = {}; o.a = q; o.p = q; same.push(o); var d = {}; for (var z = 0; z < q; z++) d["f" + z] = z; d.p = q * 2; diff.push(d); var h = Object.create(proto); h.own = q; inh.push(h); }
log("props", readAll(same, 9), readAll(diff, 9), readAll(inh, 9), readAll(same, 1));
var g = { p: 1 }; var gs = [g, g, g];
function readSome(objs) { var s = 0; for (var i = 0; i < 3; i++) { s = s + objs[i].p; if (i === 0) Object.defineProperty(g, "p", { get: function () { return 100; } }); } return s; }
log("getter-mid-loop", readSome(gs));
function writeAll(objs, n) { for (var i = 0; i < n; i++) { objs[i].w = i * 3; } var s = 0; for (var j = 0; j < n; j++) s = s + objs[j].w; return s; }
var ws = [{ w: 0 }, { w: 0 }, { w: 0, x: 1 }];
Object.defineProperty(ws[1], "w", { value: 5, writable: false });
log("writes", writeAll(ws, 3), ws[1].w);
function elems(a) { var s = 0; for (var i = -1; i < a.length + 1; i++) { var v = a[i]; s = s + (v === undefined ? 1000 : v); } return s; }
var holes = [1, , 3]; Array.prototype[1] = 50;
log("elems", elems([1, 2, 3]), elems(holes), elems(new Int8Array([1, -2, 3])), elems(new Float64Array([0.5, 1.5])));
delete Array.prototype[1];
function bigElems(a) { var s = 0n; for (var i = 0; i < 2; i++) { s = s + a[i]; } return s; }
log("bigint-ta", bigElems(new BigInt64Array([5n, 7n])));
var sparse = []; sparse[100000] = 9; sparse[3] = 4;
function sumSparse(a) { var s = 0; for (var i = 0; i < 6; i++) { var v = a[i]; if (v !== undefined) s = s + v; } return s + a[100000]; }
log("sparse", sumSparse(sparse));

// Bitwise, equality and branches on every kind of operand.
function bits(a, b) { return [a | b, a & b, a ^ b, a << b, a >> b, a >>> b].join(","); }
log("bits", bits(5, 3), bits(-1, 31), bits(2.9, 1.1), bits("6", "1"), bits(4294967296, 0), bits(NaN, 1));
function bigBits(a, b) { return [a | b, a & b, a ^ b, a << b, a >> b].join(","); }
log("bits-big", bigBits(6n, 1n), catchName(function () { return bits(6n, 1n); }), catchName(function () { return bits(1n, 1); }));
function eqs(a, b) { return (a === b) + "" + (a !== b); }
log("eq", eqs(1, 1), eqs(1, 1.0), eqs(NaN, NaN), eqs(0, -0), eqs("a", "a"), eqs({}, {}), eqs(null, undefined), eqs(1n, 1n));
function truthy(v) { var n = 0; do { n++; if (v) break; } while (n < 3); return n; }
log("truthy", truthy(0), truthy(""), truthy("x"), truthy({}), truthy(NaN), truthy(1n), truthy(0n));
function subMul(a, b) { return (a - b) + "|" + (a * b); }
log("sub-mul", subMul(5, 3), subMul(0, 5), subMul(-0, 1), subMul(0, -5), subMul(2147483647, -2), subMul("7", 2), subMul(5n, 2n));
log("sub-mul-mix", catchName(function () { return subMul(1n, 1); }));

// A private field: such a function stays in the main loop.
class P { #x = 3; sum(n) { var s = 0; for (var i = 0; i < n; i++) s = s + this.#x; return s; } }
log("private", new P().sum(4), catchName(function () { return P.prototype.sum.call({}, 1); }));

// An allocating loop: the GC's safe points are the back-edges.
function alloc(n) { var last = null; for (var i = 0; i < n; i++) { last = { v: i, prev: last }; } var s = 0; while (last !== null) { s = s + last.v; last = last.prev; } return s; }
log("alloc", alloc(2000));

// A generator driving a lite function.
function* gen(n) { for (var i = 0; i < n; i++) yield fib(i); }
log("gen", Array.from(gen(8)).join(","));

// The recursion limit is the main loop's.
function deep(n) { return n === 0 ? 0 : 1 + deep(n - 1); }
log("deep", deep(3000));
console.log(out.join("\n"));
"#;

fn node_output(source: &str) -> Vec<String> {
    let out = std::process::Command::new("node")
        .arg("-e")
        .arg(source)
        .output()
        .expect("node on PATH");
    assert!(out.status.success(), "node failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout)
        .expect("node output is UTF-8")
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn lite_loop_matches_node() {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    assert_eq!(lines, node_output(SOURCE));
}

/// The same program in fresh children: the interpreter alone (where the
/// lite loop runs), with the lite loop off, with the superinstructions off,
/// with the JIT compiling eagerly, and under GC stress.
#[test]
fn lite_loop_all_modes() {
    let exe = std::env::current_exe().expect("test exe path");
    for (name, env) in [
        ("nojit", &[("ZIPP_NOJIT", "1")][..]),
        ("no-lite", &[("ZIPP_NOJIT", "1"), ("ZIPP_NO_LITE", "1")][..]),
        ("no-fuse", &[("ZIPP_NOJIT", "1"), ("ZIPP_NO_INTERP_FUSE", "1")][..]),
        ("eager-jit", &[("ZIPP_JIT_THRESHOLD", "1")][..]),
        ("gc", &[("ZIPP_GC_STRESS", "1"), ("ZIPP_NOJIT", "1")][..]),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["lite_loop_matches_node", "--exact"]);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("spawn mode child");
        assert!(
            out.status.success(),
            "{name} failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
