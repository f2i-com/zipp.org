//! `Math.<op>(…)` on Number operands, which are computed without the
//! argument snapshot, against the same calls on operands that must be
//! coerced (in order, each once) and on a replaced method. The expected
//! lines are Node's.

const SOURCE: &str = r##"
var out = [];
var log = function () { out.push(Array.prototype.join.call(arguments, " ")); };
function show(v) { return Object.is(v, -0) ? "-0" : String(v); }
// Transcendentals may differ from V8 in the last place; compare to 12 digits.
function near(s) { var n = Number(s); return isFinite(n) && n !== 0 && !Number.isInteger(n) ? n.toPrecision(12) : s; }
var xs = [0, -0, 1, -1, 0.5, -0.5, 2.5, -2.5, 1e-300, 1e300, 123.456, -7.25, 2147483648, 4294967297,
  NaN, Infinity, -Infinity, 3, 1 / 3, 9007199254740993];
var unary = ["abs", "floor", "ceil", "round", "trunc", "sign", "sqrt", "cbrt", "exp", "log", "log2", "log10",
  "expm1", "log1p", "sin", "cos", "tan", "asin", "acos", "atan", "sinh", "cosh", "tanh", "asinh", "acosh",
  "atanh", "clz32", "fround"];
for (var u = 0; u < unary.length; u++) {
  var row = [];
  for (var i = 0; i < xs.length; i++) {
    var x = xs[i];
    switch (unary[u]) {
      case "abs": row.push(show(Math.abs(x))); break;
      case "floor": row.push(show(Math.floor(x))); break;
      case "ceil": row.push(show(Math.ceil(x))); break;
      case "round": row.push(show(Math.round(x))); break;
      case "trunc": row.push(show(Math.trunc(x))); break;
      case "sign": row.push(show(Math.sign(x))); break;
      case "sqrt": row.push(show(Math.sqrt(x))); break;
      case "cbrt": row.push(show(Math.cbrt(x))); break;
      case "exp": row.push(show(Math.exp(x))); break;
      case "log": row.push(show(Math.log(x))); break;
      case "log2": row.push(show(Math.log2(x))); break;
      case "log10": row.push(show(Math.log10(x))); break;
      case "expm1": row.push(show(Math.expm1(x))); break;
      case "log1p": row.push(show(Math.log1p(x))); break;
      case "sin": row.push(show(Math.sin(x))); break;
      case "cos": row.push(show(Math.cos(x))); break;
      case "tan": row.push(show(Math.tan(x))); break;
      case "asin": row.push(show(Math.asin(x))); break;
      case "acos": row.push(show(Math.acos(x))); break;
      case "atan": row.push(show(Math.atan(x))); break;
      case "sinh": row.push(show(Math.sinh(x))); break;
      case "cosh": row.push(show(Math.cosh(x))); break;
      case "tanh": row.push(show(Math.tanh(x))); break;
      case "asinh": row.push(show(Math.asinh(x))); break;
      case "acosh": row.push(show(Math.acosh(x))); break;
      case "atanh": row.push(show(Math.atanh(x))); break;
      case "clz32": row.push(show(Math.clz32(x))); break;
      default: row.push(show(Math.fround(x)));
    }
  }
  log(unary[u], row.map(near).join(","));
}
var pairs = [[2, 10], [2, 0.5], [-8, 1 / 3], [1, NaN], [-1, Infinity], [1, -Infinity], [NaN, 0], [0, -1], [-0, -1],
  [-0, -3], [Infinity, -0.5], [2.5, 2147483649], [0xffffffff, 0x9e3779b1], [-1.5e10, 3.7], [1e20, 1e20], [0, 0], [-0, -0]];
var pw = [], at = [], im = [];
for (var p = 0; p < pairs.length; p++) {
  var a = pairs[p][0], b = pairs[p][1];
  pw.push(show(Math.pow(a, b))); at.push(show(Math.atan2(a, b))); im.push(show(Math.imul(a, b)));
}
log("pow", pw.map(near).join(",")); log("atan2", at.map(near).join(",")); log("imul", im.join(","));
var calls = [];
function tracked(name, v) { return { valueOf: function () { calls.push(name); return v; } }; }
log("extra args ignored", Math.floor(2.7, tracked("floor extra", 1)), Math.sqrt(16, tracked("sqrt extra", 1)),
  Math.pow(2, 3, tracked("pow extra", 1)), Math.imul(3, 5, tracked("imul extra", 1)), calls.join() || "none");
calls = [];
log("coerced", Math.floor(tracked("f", 2.5)), Math.pow(tracked("pa", 2), tracked("pb", 5)), Math.atan2(tracked("aa", 1), 1),
  Math.imul(3, tracked("ib", 7)), Math.pow("3", "2"), Math.floor("7.9"), Math.imul("6", true), calls.join());
log("missing", show(Math.floor()), show(Math.pow(2)), show(Math.atan2(1)), show(Math.imul(5)), show(Math.sqrt(undefined)), show(Math.abs(null)));
try { Math.floor(1n); log("bigint", "no throw"); } catch (e) { log("bigint", e.constructor.name); }
try { Math.pow(2, Symbol()); log("symbol", "no throw"); } catch (e) { log("symbol", e.constructor.name); }
var replaced = [];
var saved = { floor: Math.floor, pow: Math.pow };
Math.floor = function (v) { replaced.push("floor " + v); return 42; };
Math.pow = function (a, b) { replaced.push("pow " + a + "," + b); return -1; };
var seen = [];
for (var k = 0; k < 3; k++) seen.push(Math.floor(k + 0.5), Math.pow(k, 2));
Math.floor = saved.floor; Math.pow = saved.pow;
log("replaced", seen.join(), replaced.join(";"), Math.floor(9.5), Math.pow(3, 3));
var order = [];
var sneaky = { valueOf: function () { order.push("valueOf"); Math.floor = function () { return "swapped"; }; return 1.5; } };
var r1 = Math.floor(sneaky * 2);
var r2 = Math.floor(3.5);
Math.floor = saved.floor;
log("captured before args", r1, r2, order.join());
console.log(out.join("\n"));
"##;

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
fn math_on_number_operands_matches_node() {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    assert_eq!(lines, node_output(SOURCE));
}
