//! The no-copy arms of the string methods: trims (the receiver itself when
//! nothing is trimmed), ASCII and short case mapping, padding with string
//! fillers, and small repeats, next to the inputs that still take the
//! general path (coerced arguments, lone surrogates, long results, the
//! RangeErrors). A result that is its receiver must stay immutable when it
//! is appended to. The expected lines are Node's.

const SOURCE: &str = r##"
var out = [];
var log = function () { out.push(Array.prototype.join.call(arguments, " ")); };
function attempt(label, f) {
  try { log(label, "ok", JSON.stringify(f())); } catch (e) { log(label, "threw", e.constructor.name); }
}
var ws = ["", "x", " x ", "\t\n x  ", "﻿ab　", " y ", "  ", "a b", "\ud800 ", " \udc00", "plain"];
var row = [];
for (var i = 0; i < ws.length; i++) row.push(JSON.stringify([ws[i].trim(), ws[i].trimStart(), ws[i].trimEnd()]));
log("trims", row.join(" "));
var cases = ["abc", "ABC", "aBc1!", "", "123", "été", "STRAßE", "\ud800a", "mIxEd CaSe"];
row = [];
for (var i = 0; i < cases.length; i++) row.push(cases[i].toUpperCase() + "/" + cases[i].toLowerCase());
log("case", JSON.stringify(row));
row = [];
var pads = [[5, undefined], [5, "0"], [7, "ab"], [2, "xyz"], [6, ""], [-3, "z"], [8, 12], [8, { toString: function () { return "#"; } }], [4.5, "-"], [9, "é"]];
for (var i = 0; i < pads.length; i++) row.push(JSON.stringify(["abc".padStart(pads[i][0], pads[i][1]), "abc".padEnd(pads[i][0], pads[i][1])]));
log("pad", row.join(" "));
log("pad unicode", "é".padStart(4, "ab"), "x".padEnd(3, "😀"));
row = [];
var reps = [0, 1, 3, 2.7, "2", null];
for (var i = 0; i < reps.length; i++) row.push(JSON.stringify(["ab".repeat(reps[i]), "é😀".repeat(reps[i]), "".repeat(reps[i])]));
log("repeat", row.join(" "));
attempt("repeat negative", function () { return "a".repeat(-1); });
attempt("repeat infinite", function () { return "a".repeat(Infinity); });
attempt("repeat long", function () { return "abcd".repeat(5000).length; });
attempt("pad long", function () { return "a".padStart(9000, "xy").length; });
attempt("pad huge", function () { return "a".padStart(2147483647, "xy").length; });
// Results that are their receivers, then appended to.
var base = "steady";
var t = base.trim(), u = base.toLowerCase(), p = base.padStart(3, "z");
var acc = t;
for (var i = 0; i < 5; i++) acc += i;
var acc2 = u + "!" + "?";
var acc3 = p;
for (var i = 0; i < 3; i++) acc3 += "#";
var acc4 = u + "" + "?" + "!";
var acc5 = "" + t + base.length + "x";
var acc6 = base.trimEnd() + "" + base.trimStart();
log("aliases", base, t, u, p, acc, acc2, acc3, acc4, acc5, acc6, t === base, u === base, p === base);
var big = "";
for (var i = 0; i < 300; i++) big += " w" + i;
var bt = big.trim();
for (var i = 0; i < 4; i++) bt += "|";
log("long trim", big.length, bt.length, big.slice(-4), bt.slice(-6));
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
fn short_string_method_paths_match_node() {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    assert_eq!(lines, node_output(SOURCE));
}
