//! `switch` cases on integer and string literals, which the interpreter
//! runs as one superinstruction each (the literal's load, the `===` and the
//! branch): every kind of discriminant must take the case `===` picks, with
//! fallthrough, a default in the middle, a case reached by a jump into the
//! middle of the chain, and the same comparisons outside a switch (`||`
//! keeps the comparison's value). The expected lines are Node's.

const SOURCE: &str = r##"
var out = [];
function ints(v) {
  var s = "";
  switch (v) {
    case 0: s += "zero";
    case 1: s += "one"; break;
    case -1: s += "minus"; break;
    default: s += "def";
    case 7: s += "seven"; break;
    case 2147483647: s += "max"; break;
    case -2147483648: s += "min"; break;
  }
  return s;
}
function strs(v) {
  switch (v) {
    case "a": return "A";
    case "": return "empty";
    case "let": return "kw";
    case "0": return "str0";
    default: return "other";
  }
}
var big = 2147483647;
var values = [0, -0, 1, 1.0, 0.5 * 2, -1, 7, 7.5, NaN, Infinity, big, big + 1, -big - 1,
  "0", "1", "a", "", "let", "le" + "t", true, false, null, undefined, [1], {}, 1n, 7n,
  new Number(1), new String("a"), Symbol.iterator];
for (var i = 0; i < values.length; i++) {
  var v = values[i];
  var name = typeof v === "symbol" ? "sym" : typeof v === "bigint" ? v + "n" : (Object.is(v, -0) ? "-0" : String(v));
  out.push(name + ":" + ints(v) + "/" + strs(v));
}
// The same comparison outside a switch: `||` keeps the boolean.
function orq(x) { return x === 3 || x === "s"; }
out.push([orq(3), orq(3.0), orq("s"), orq(4), orq("3")].join());
// A loop over a switch, many times, so a compiled tier sees it too.
var acc = 0;
for (var k = 0; k < 20000; k++) {
  switch (k % 9) {
    case 0: acc += 1; break;
    case 3: acc += 3;
    case 4: acc ^= 4; break;
    case 8: acc -= 8; break;
    default: acc = (acc * 3) | 0;
  }
  switch (k & 1 ? "odd" : "even") {
    case "odd": acc = (acc + 1) | 0; break;
    case "even": acc = (acc ^ 2) | 0; break;
  }
}
out.push("acc " + acc);
// Case expressions with effects are not literals: evaluated in order.
var log = [];
function t(n) { log.push(n); return n; }
switch (2) { case t(1): log.push("x"); case t(2): log.push("hit"); case t(3): log.push("fall"); }
out.push(log.join());
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
fn switch_case_literals_worker() {
    if std::env::var_os("ZIPP_SWITCH_CASE_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    assert_eq!(lines, node_output(SOURCE));
}

#[test]
fn switch_case_literals_match_node() {
    if std::env::var_os("ZIPP_SWITCH_CASE_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    for (mode, nojit, nofuse) in [("default", false, false), ("interpreter", true, false), ("interpreter-unfused", true, true)] {
        let mut command = std::process::Command::new(&exe);
        command
            .args(["switch_case_literals_worker", "--exact", "--nocapture"])
            .env("ZIPP_SWITCH_CASE_WORKER", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_NO_INTERP_FUSE");
        if nojit {
            command.env("ZIPP_NOJIT", "1");
        }
        if nofuse {
            command.env("ZIPP_NO_INTERP_FUSE", "1");
        }
        let out = command.output().expect("spawn the worker");
        assert!(
            out.status.success(),
            "{mode} diverged from node:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
