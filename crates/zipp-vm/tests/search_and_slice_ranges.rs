//! String search on a receiver holding a lone surrogate (searched in its own
//! WTF-8 bytes, never a lossy copy) and `Array.prototype.slice` deciding the
//! dense copy from the sliced range alone. The expected lines are Node's.

const SOURCE: &str = r#"
var out = [];
var log = function () { out.push(Array.prototype.join.call(arguments, " ")); };
var methods = ["indexOf", "lastIndexOf", "includes", "startsWith", "endsWith"];
function probe(label, s, needles) {
  for (var m = 0; m < methods.length; m++) {
    var row = [];
    for (var n = 0; n < needles.length; n++) {
      for (var p = -1; p <= s.length + 1; p++) row.push(String(s[methods[m]](needles[n], p)));
      row.push(String(s[methods[m]](needles[n])));
    }
    log(label, methods[m], row.join(","));
  }
}
var base = "abé日x😀yx";
var lone = base.slice(0, 3) + "\ud800" + base.slice(3) + "\udc00x�";
var needles = ["x", "é", "日x", "😀", "�", "x�", "", "\ud800", "\ude00y", "zz"];
probe("lone", lone, needles);
probe("wellformed", base + "�", needles);
var pairSplit = "😀😀";
probe("pair", pairSplit, ["😀", "\ude00", "\ud83d"]);
// A find-all loop over a long receiver with one lone surrogate.
var long = ("abcéx").repeat(2000);
long = long.slice(0, 7) + "\udbff" + long.slice(7);
var hits = 0, at = long.indexOf("x");
while (at >= 0) { hits++; at = long.indexOf("x", at + 1); }
var back = 0, last = long.lastIndexOf("x");
while (last >= 0) { back++; last = last === 0 ? -1 : long.lastIndexOf("x", last - 1); }
log("find-all", hits, back);

// slice: a hole outside the range keeps the dense copy exact; one inside
// reads through the prototype; a hole sliced out stays a hole.
var a = [0, 1, 2, 3, 4, 5, 6, 7];
delete a[1];
log("outside", JSON.stringify(a.slice(3, 6)), a.slice(3, 6).length);
log("inside", JSON.stringify(a.slice(0, 3)), 1 in a.slice(0, 3));
Array.prototype[1] = "proto";
log("inside-proto", JSON.stringify(a.slice(0, 3)), a.slice(-8, 2).join("|"));
delete Array.prototype[1];
var big = []; for (var i = 0; i < 1000; i++) big.push(i); delete big[999];
log("big", big.slice(8, 24).join(","), big.slice(990).length, 9 in big.slice(990));
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
fn search_and_slice_match_node() {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    assert_eq!(lines, node_output(SOURCE));
}
