//! The interpreter's superinstructions (`FusedCode`: `AddInt`+`Move`,
//! `LoadGlobal`+`LoadGlobal`, `LoadInt`+`Bitwise`, a compare and its branch,
//! a bound loaded and compared, `LoadInt`+`Add`, a counter step and its
//! back-edge, each run as one dispatch) give exactly what the two
//! instructions give, on their number fast paths and on every other operand:
//! strings and objects coerced (a user `valueOf` runs once, in order),
//! BigInts, a throw in either half, an undeclared, uninitialized or
//! getter-backed global, a jump into the second half. The expected lines are
//! Node 24's.

const SOURCE: &str = r#"
var out = [];
var log = function () { out.push(Array.prototype.join.call(arguments, " ")); };
function catchName(f) { try { return f(); } catch (e) { return e.constructor.name; } }

// AddInt + Move: `i++` / `x += k` statements on every kind of operand.
function addMove(v, n) {
  var r = [];
  for (var i = 0; i < n; i++) {
    var x = v;
    x++;
    var y = x;
    r.push(typeof y === "bigint" ? y + "n" : String(y));
  }
  return r.join(",");
}
log("am-int", addMove(5, 3), addMove(2147483647, 2), addMove(-0, 2), addMove(1.5, 2));
log("am-str", addMove("41", 2), addMove("x", 2), addMove("", 2), addMove(null, 2), addMove(undefined, 2), addMove(true, 2));
log("am-big", addMove(10n, 2), addMove(-1n, 2));
var vo = 0;
log("am-obj", addMove({ valueOf: function () { vo++; return 7; } }, 3), vo);
log("am-throw", catchName(function () { return addMove({ valueOf: function () { throw new RangeError("v"); } }, 2); }));
log("am-sym", catchName(function () { return addMove(Symbol("s"), 2); }));
// A `for` counter whose increment the loop also copies out.
var last = "";
for (var k = 0; k < 5; k++) { last = last + k; }
log("am-loop", last, k);

// LoadGlobal + LoadGlobal: adjacent reads of script globals.
var g1 = 3, g2 = 4, gs = 0;
for (var j = 0; j < 4; j++) { gs = gs + g1 * g2 + j; }
log("lg", gs);
Object.defineProperty(globalThis, "gGet", { get: function () { return 10; }, configurable: true });
var gsum = 0;
for (var j2 = 0; j2 < 3; j2++) { gsum = gsum + g1 + gGet; }
log("lg-getter", gsum);
log("lg-undeclared", catchName(function () { var s = 0; for (var q = 0; q < 2; q++) { s = g1 + gNope; } return s; }));
var gLater;
log("lg-undef", (function () { var s = []; for (var q = 0; q < 2; q++) { s.push(g1 + "," + gLater); } return s.join(";"); })());

// LoadInt + Bitwise: `x | 0`, `x & 255`, `x >>> 1` on every kind of operand.
function bits(v, n) {
  var r = [];
  for (var i = 0; i < n; i++) {
    r.push((v | 0) + ":" + (v & 255) + ":" + (v >>> 1) + ":" + (v << 3) + ":" + (v >> 2) + ":" + (v ^ 5));
  }
  return r.join(",");
}
log("bw-num", bits(1234567, 2), bits(-7.9, 2), bits(4294967295, 2), bits(1e21, 2), bits(NaN, 2), bits(-0, 2));
log("bw-str", bits("12", 2), bits("0x10", 2), bits("x", 2), bits(null, 2), bits(undefined, 2), bits(true, 2));
var bo = 0;
log("bw-obj", bits({ valueOf: function () { bo++; return 9; } }, 2), bo);
log("bw-big", catchName(function () { return bits(5n, 2); }));
log("bw-throw", catchName(function () { return bits({ valueOf: function () { throw new TypeError("b"); } }, 2); }));

// Eq/Ne/typeof and the branch on the result (`if (x === y)`), forward only;
// a `do … while` closes its loop with a BACKWARD conditional, never fused.
function cmpBranch(list) {
  var r = [];
  for (var i = 0; i < list.length; i++) {
    var x = list[i], s = "";
    if (x === 3) s += "a"; else s += "b";
    if (x !== "3") s += "c"; else s += "d";
    if (x === null) s += "n";
    if (typeof x === "number") s += "N"; else if (typeof x === "string") s += "S"; else if (typeof x !== "object") s += "O";
    if (typeof x === "function") s += "F";
    r.push(s);
  }
  return r.join(",");
}
var o1 = {};
log("cmp", cmpBranch([3, "3", 3.0, -0, 0, NaN, null, undefined, o1, 3n, function () {}, true]));
log("cmp-same", (function () { var n = 0, a = [o1, {}, o1]; for (var i = 0; i < 3; i++) { if (a[i] === o1) n++; } return n; })());
log("cmp-nan", (function () { var n = 0; for (var i = 0; i < 3; i++) { var z = NaN; if (z !== z) n++; } return n; })());
var dw = 0, dwi = 0;
do { dw += dwi; dwi++; } while (dwi !== 5);
log("do-while", dw, dwi);

// A jump into the second instruction of a pair runs it alone.
function jumpIn(c, n) {
  var r = 0, t = 0;
  for (var i = 0; i < n; i++) {
    t = c ? 7 : i;
    r = r + (t | 0);
  }
  return r;
}
log("jump", jumpIn(true, 5), jumpIn(false, 5));

// LoadInt + JumpIfNotLt/Le: a literal bound, on every kind of operand.
function litBound(v) {
  var r = [];
  for (var i = 0; i < 2; i++) {
    r.push((v < 3 ? "lt" : "ge") + (v <= 3 ? "le" : "gt"));
  }
  return r.join(",");
}
log("lit", litBound(2), litBound(3), litBound(4), litBound(2.5), litBound(NaN), litBound(-0), litBound(-Infinity));
log("lit-str", litBound("2"), litBound("3"), litBound("x"), litBound(null), litBound(undefined), litBound(true));
log("lit-big", litBound(2n), litBound(3n), litBound(4n));
var lo = 0;
log("lit-obj", litBound({ valueOf: function () { lo++; return 1; } }), lo);
log("lit-throw", catchName(function () { return litBound({ valueOf: function () { throw new RangeError("l"); } }); }));
log("lit-sym", catchName(function () { return litBound(Symbol("s")); }));

// LoadGlobal + JumpIfNotLt/Le: a bound held in a script global.
var GB = 3, gbo = 0;
function globBound(n) { var c = 0; for (var i = 0; i < GB; i++) c += n; for (var j = 0; j <= GB; j++) c += n; return c; }
log("glob", globBound(1));
GB = 2.5; log("glob-float", globBound(1));
GB = "2"; log("glob-str", globBound(1));
GB = 2n; log("glob-big", globBound(1));
GB = { valueOf: function () { gbo++; return 2; } }; log("glob-obj", globBound(1), gbo);
GB = 3;
log("glob-tdz", catchName(function () { var c = 0; for (var i = 0; i <= LATE; i++) c++; return c; }));
let LATE = 2;
log("glob-late", (function () { var c = 0; for (var i = 0; i <= LATE; i++) c++; return c; })());
log("glob-undeclared", catchName(function () { for (var i = 0; i < NOPE; i++) {} }));
Object.defineProperty(globalThis, "GG", { get: function () { return 4; }, configurable: true });
log("glob-getter", (function () { var c = 0; for (var i = 0; i < GG; i++) c++; return c; })());

// LoadInt + Add: `x + 1` where `x` may be anything.
function addLit(v) {
  var r = [];
  for (var i = 0; i < 2; i++) {
    var y = v + 1;
    r.push(typeof y === "bigint" ? y + "n" : String(y));
  }
  return r.join(",");
}
log("al", addLit(5), addLit(2147483647), addLit(-0), addLit(1.5), addLit("a"), addLit(""), addLit(null), addLit(undefined), addLit(true));
var ao = 0;
log("al-obj", addLit({ valueOf: function () { ao++; return 7; } }), addLit({ toString: function () { return "s"; }, valueOf: undefined }), ao);
log("al-big", catchName(function () { return addLit(1n); }));
log("al-arr", addLit([1, 2]), addLit([]));

// AddInt + Jump: a counter step and its back-edge.
log("step", (function () {
  var bi = []; for (var q = 0n; q < 3n; q++) bi.push(String(q));
  var fl = []; for (var q2 = 0.5; q2 < 3; q2++) fl.push(q2);
  var ov = []; for (var q3 = 2147483646; q3 < 2147483649; q3++) ov.push(q3);
  var dn = []; for (var q5 = -2147483647; q5 > -2147483650; q5--) dn.push(q5);
  var sc = 0, q4; for (q4 = "1"; q4 < 4; q4++) sc++;
  var keep = null; for (var q6 = 0; q6 < 20000; q6++) keep = [q6, { q6: q6 }];
  return [bi.join(","), fl.join(","), ov.join(","), dn.join(","), sc, typeof q4, keep[1].q6].join(" ");
})());

// LoadGlobal + AddInt: a global callee and its `n - 1` argument; Add +
// Return: the sum returned, also from inside `try`/`finally`.
function fibg(n) { return n < 2 ? n : fibg(n - 1) + fibg(n - 2); }
log("fibg", fibg(15), fibg("9"), fibg(7.5), fibg(true), catchName(function () { return fibg(3n); }));
function sum2(a, b) { return a + b; }
var so = 0;
log("ret", sum2(1, 2), sum2(2147483647, 1), sum2("a", 1), sum2(1, "b"), sum2(null, undefined), sum2([1], [2]),
  sum2({ valueOf: function () { so++; return 40; } }, 2), so, sum2(1n, 2n), catchName(function () { return sum2(1n, 1); }));
var fins = [];
function finRet(a, b) { try { return a + b; } finally { fins.push("f" + a); } }
log("ret-finally", finRet(1, 2), finRet("x", "y"), fins.join(","),
  catchName(function () { return finRet({ valueOf: function () { throw new EvalError("e"); } }, 1); }), fins.join(","));
var gstep = 10;
function useStep(k) { var r = []; for (var i = 0; i < 2; i++) { var g = gstep; r.push(String(k - 1), String(k + 1)); } return r.join(","); }
log("lg-add", useStep(5), useStep(2147483647), useStep(-2147483648), useStep(0.5), useStep("7"), catchName(function () { return useStep(1n); }));
log("lg-add-tdz", catchName(function () { var c = LATE2; return c - 1; }));
let LATE2 = 1;
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
fn interp_fuse_matches_node() {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    assert_eq!(lines, node_output(SOURCE));
}

/// The same program in fresh children: the interpreter alone (where the
/// superinstructions run everywhere), with the superinstructions off, with
/// the JIT compiling eagerly, and under GC stress.
#[test]
fn interp_fuse_all_modes() {
    let exe = std::env::current_exe().expect("test exe path");
    for (name, env) in [
        ("nojit", &[("ZIPP_NOJIT", "1")][..]),
        ("no-fuse", &[("ZIPP_NOJIT", "1"), ("ZIPP_NO_INTERP_FUSE", "1")][..]),
        ("eager-jit", &[("ZIPP_JIT_THRESHOLD", "1")][..]),
        ("gc", &[("ZIPP_GC_STRESS", "1"), ("ZIPP_NOJIT", "1")][..]),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["interp_fuse_matches_node", "--exact"]);
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
