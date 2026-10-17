//! Unit positions in long non-ASCII strings found from scattered starting
//! points (the seek marks), and case mapping in one pass for short
//! well-formed text next to the measured path for long text and text with
//! lone surrogates. The expected lines are Node's.

const SOURCE: &str = r##"
var out = [];
var log = function () { out.push(Array.prototype.join.call(arguments, " ")); };
function h(s) { var x = 2166136261; for (var i = 0; i < s.length; i++) x = Math.imul(x ^ s.charCodeAt(i), 16777619); return x >>> 0; }

// A long two-byte string with astral pairs; searches and reads from scattered positions.
var pieces = ["été", "日本", "😀", "abc", "Ж", "\u{1F4A9}x", "z0"];
var big = "";
for (var i = 0; i < 9000; i++) big += pieces[(i * 5 + (i >> 3)) % pieces.length] + String.fromCharCode(97 + i % 26);
var needles = ["日本", "😀", "abc", "z0q", "Ж", "\ude00", "\ud83d"];
var acc = 0, hits = 0;
for (var i = 0; i < 4000; i++) {
  var from = (i * 7919) % big.length;
  var n = needles[i % needles.length];
  var a = big.indexOf(n, from), b = big.lastIndexOf(n, from);
  if (a >= 0) { hits++; acc = (acc * 31 + a - from) | 0; }
  if (b >= 0) { hits++; acc = (acc * 31 + from - b) | 0; }
  acc = (acc + big.charCodeAt(from) + (big.codePointAt(from) | 0)) | 0;
  if (big.includes(n, from)) acc ^= 1;
}
log("seek", big.length, hits, acc, big.slice(17001, 17011).length, h(big.slice(9000, 9100)));
var grown = big;
for (var i = 0; i < 50; i++) grown += "ā😀" + i;
var row = [];
for (var i = 0; i < 40; i++) { var p = (i * 104729) % grown.length; row.push(grown.charCodeAt(p), grown.indexOf("ā", p)); }
log("grown", grown.length, h(row.join(",")));
var cut = big.charCodeAt(19999) >= 0xd800 && big.charCodeAt(19999) < 0xdc00 ? 19999 : 20000;
var seam = big.slice(0, cut);
for (var i = 0; i < 3000; i += 7) seam.indexOf("z0", (i * 31) % cut);
seam += "\ud83d";
seam += "\ude00tail";
log("seam", seam.length, seam.charCodeAt(cut), seam.codePointAt(cut), seam.indexOf("tail", 1234), seam.lastIndexOf("😀", cut + 1), seam.indexOf("\ude00", cut - 3));

// Case mapping: final sigma, length-changing and special cases, lone surrogates, long text.
var words = ["straße", "İstanbul", "ΟΔΥΣΣΕΥΣ", "ΣΊΣΥΦΟΣ ΚΑΙ",
  "ΊΣ", "İ", "ﬃ", "ŉ", "ǅ", "ΐ", "ẞ", "Σ", "aΣ", "aΣb", "aΣ.", "µÿı",
  "Дмитрий", "\u{10400}\u{10428}", "\ud800xΣ", "xΣ\udc00", "é\ud83d"];
var cases = [];
for (var i = 0; i < words.length; i++) {
  var w = words[i];
  cases.push(h(w.toUpperCase()) + ":" + w.toUpperCase().length + "/" + h(w.toLowerCase()) + ":" + w.toLowerCase().length);
}
log("case", cases.join(" "));
var long = "";
for (var i = 0; i < 9000; i++) long += words[i % 17] + " ";
log("long case", long.length, h(long.toUpperCase()), h(long.toLowerCase()), h(long.toLowerCase().toUpperCase()));

// URI encoding: every ASCII character through both encoders, and non-ASCII text.
var ascii = "";
for (var i = 0; i < 128; i++) ascii += String.fromCharCode(i);
var uri = [encodeURIComponent(ascii), encodeURI(ascii)];
var texts = ["héllo wörld", "日本語/テキスト", "emoji 😀🎉", "100% sure", "#frag?x=1&y=2", "Ω≈ç√", ""];
for (var i = 0; i < texts.length; i++) {
  var e = encodeURIComponent(texts[i]), e2 = encodeURI(texts[i]);
  uri.push(e, e2, decodeURIComponent(e) === texts[i], decodeURI(e2) === texts[i]);
}
log("uri", h(uri.join("|")), uri[0].length, uri[1].length, uri.slice(2, 6).join(" "));
try { encodeURIComponent("a\ud800b"); log("lone", "no"); } catch (e) { log("lone", e.constructor.name); }
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
fn unicode_seek_and_case_match_node() {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    assert_eq!(lines, node_output(SOURCE));
}
