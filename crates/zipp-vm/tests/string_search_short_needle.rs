//! A positional `indexOf` / `includes` / `lastIndexOf` searches a needle of
//! up to eight bytes by its first byte and a compare instead of memmem's
//! per-call searcher. Every needle length on both sides of that bound,
//! overlapping and self-similar needles, matches at the ends, positions
//! before, inside and past the string, and non-ASCII haystacks and needles
//! must give Node's answers.

const SOURCE: &str = r##"
var out = [];
var log = function () { out.push(Array.prototype.join.call(arguments, " ")); };
var hays = [
  "aaaaaaaaaabaaaaaaaaaab",
  "abcabcabcabdabcabcabcabd;rec1:7;rec22:14;\nrec333:21;",
  "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxy",
  "héllo wörld héllo wörld ünïcode ✓ check ✓ done",
  "a\u{1F600}b\u{1F600}a\u{1F600}b\u{1F600}c",
  ""
];
var needles = ["a", "b", "ab", "aab", "aaab", "aaaab", "aaaaaab", "aaaaaaab", "aaaaaaaab", "aaaaaaaaab",
  "abc", "abd", "abcabd", "abcabcabd", "rec", "rec3", ";\nrec", "x", "xy", "xxxxxxxxy", "xxxxxxxxxy",
  "é", "héllo", "wörld ü", "✓", "✓ d", "\u{1F600}", "\u{1F600}c", "b\u{1F600}a", "zz", ""];
var positions = [-5, 0, 1, 3, 7, 11, 20, 40, 1000, NaN, 2.5, Infinity, -Infinity];
for (var h = 0; h < hays.length; h++) {
  var s = hays[h], line = [];
  for (var n = 0; n < needles.length; n++) {
    var t = needles[n], cells = [];
    for (var p = 0; p < positions.length; p++) {
      var at = positions[p];
      cells.push(s.indexOf(t, at) + "/" + s.lastIndexOf(t, at) + "/" + (s.includes(t, at) ? 1 : 0));
    }
    line.push(cells.join(","));
  }
  log("hay" + h, line.join(" | "));
}
// A delimiter loop, the shape the change is for.
var rec = ""; for (var i = 0; i < 2000; i++) rec += "rec" + i + ":" + (i * 7 % 1000) + ";";
var sum = 0, pos = 0, count = 0;
while ((pos = rec.indexOf("rec", pos)) >= 0) { count++; sum = (sum + pos) | 0; pos += 1; }
var back = 0, at = rec.length;
while (at >= 0 && (at = rec.lastIndexOf(";", at)) >= 0) { back++; at -= 1; }
log("loops", count, sum, back);
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
fn string_search_short_needle_matches_node() {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    assert_eq!(lines, node_output(SOURCE));
}
