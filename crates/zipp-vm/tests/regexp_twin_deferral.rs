//! A RegExp searches short ASCII subjects with its base program for its
//! first few execs and with its byte-optimized twin after that. Every
//! result, capture and `lastIndex` must be the same on both sides of the
//! switch, for every flag, on short and long subjects, on ASCII and
//! non-ASCII subjects through the same regex, and past the deferral table's
//! cap. The expected lines are Node's.

const SOURCE: &str = r##"
var out = [];
function fnv(h, s) { for (var i = 0; i < s.length; i++) h = Math.imul(h ^ s.charCodeAt(i), 16777619); return h >>> 0; }
function show(m) { return m === null ? "null" : JSON.stringify([m.index, Array.from(m), m.groups || null, m.indices || null]); }
var patterns = [
  ["(\\w+)=(\\d+)", ""], ["(\\w+)=(\\d+)", "g"], ["(\\w+)=(\\d+)", "y"], ["[A-Z][a-z]+", "gi"],
  ["^line\\d$", "gm"], ["a.b", "s"], ["(?<k>[a-z]{3})\\k<k>", "g"], ["(?<=\\$)\\d+", "g"],
  ["(a|ab)(c|bcd)(d*)", ""], ["\\bfoo\\b", "gu"], ["x*", "g"], ["(\\d{2})-(\\d{2})", "dg"],
  ["q[^,;]{1,8}", "gy"], ["(?:get|put|post)\\s+(\\S+)", "gi"], ["(\\w)\\1", "g"]
];
var subjects = ["key=42 other=7 end", "Hello World Again", "line1\nline2\nline3", "a\nb axb", "abcabc xyzxyz",
  "cost $12 and $345", "abcd abcbcd", "foo bar foofoo foo", "", "12-34 56-78 90-12", "q12345,q9;qabcdefghij",
  "GET /a PUT /b post /c", "aabbcc xyyz", "café key=99 ü=1", "x".repeat(5000) + " key=5000 foo"];
var h = 0, lines = [];
for (var p = 0; p < patterns.length; p++) {
  var re = new RegExp(patterns[p][0], patterns[p][1]);
  var seq = [];
  for (var round = 0; round < 8; round++) {
    var s = subjects[(p + round) % subjects.length];
    var m = re.exec(s);
    seq.push(show(m) + "@" + re.lastIndex);
    seq.push(String(re.test(s)) + "@" + re.lastIndex);
    if (round === 5) re.lastIndex = 0;
  }
  h = fnv(h, seq.join("|"));
  if (p % 4 === 0) lines.push(patterns[p][0] + "/" + patterns[p][1] + " " + seq.slice(0, 4).join(" "));
}
out.push("matrix " + h);
Array.prototype.push.apply(out, lines);
var sticky = /\w+/y, positions = [];
var text = "ab cd ef gh ij kl";
for (var i = 0; i < 12; i++) { sticky.lastIndex = i; var mm = sticky.exec(text); positions.push(mm ? mm[0] + ":" + sticky.lastIndex : "-:" + sticky.lastIndex); }
out.push("sticky walk " + positions.join(" "));
var g = /o/g, walk = [];
for (var i = 0; i < 7; i++) { var r = g.exec("foo boo"); walk.push(r ? r.index : "null"); }
out.push("global walk " + walk.join(","));
var many = 0;
for (var i = 0; i < 1500; i++) {
  var r2 = new RegExp("z" + i.toString(36) + "|(\\d+)", i & 1 ? "g" : "");
  var m2 = r2.exec("n" + i + " z" + i.toString(36));
  if (m2) many = (many * 31 + m2.index + m2[0].length) | 0;
  if (r2.test("zz" + i)) many ^= i;
}
out.push("many distinct " + many);
var mixed = /([a-z]+)(\d)/g, mixedOut = [];
var subjectsMixed = ["abc1 def2", "été1 abc2", "abc1 def2", "xyz9", "中文a1"];
for (var r = 0; r < 10; r++) { var mm2 = mixed.exec(subjectsMixed[r % subjectsMixed.length]); mixedOut.push(mm2 ? mm2[0] + "@" + mixed.lastIndex : "null@" + mixed.lastIndex); }
out.push("ascii and not " + mixedOut.join(" "));
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
fn regexp_results_match_node_across_the_twin_switch() {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    assert_eq!(lines, node_output(SOURCE));
}
