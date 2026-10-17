//! toFixed, toExponential and toPrecision at every digit count over random
//! bit patterns, the kinds of value programs format, and values built to sit
//! exactly on a tie (which JS settles away from zero), against Node.

const SOURCE: &str = r##"
var out = [];
var f64 = new Float64Array(1);
var u32 = new Uint32Array(f64.buffer);
var seed = 7;
function next() { seed = (Math.imul(seed, 1664525) + 1013904223) | 0; return seed >>> 0; }
var values = [];
while (values.length < 120) {
  u32[0] = next(); u32[1] = next() & 0x7fffffff;
  var r = f64[0];
  if (r === r && r !== Infinity && r !== 0) values.push(r);
}
for (var i = 0; i < 120; i++) {
  var k = next() % 1000000000;
  switch (i % 6) {
    case 0: values.push(k / 1000); break;
    case 1: values.push((k % 1000000) * 1.1 + 1); break;
    case 2: values.push((k % 100000 + 1) * 1e-9); break;
    case 3: values.push((k % 1000 + 1) * 1e18 + k % 997); break;
    case 4: values.push(1 / (1 + k % 1000)); break;
    default: values.push((k % 100000 + 0.5) / 128);
  }
}
for (var j = 0; j < 40; j++) values.push((2 * j + 1) / Math.pow(2, j % 20 + 1));
for (var c = 0; c < 21; c++) values.push(3 * Math.pow(5, c + 1) * Math.pow(2, c));
values.push(1.005, 1.45, 0.5, 1.5, 2.5, 9.5, 0.125, 1.25, 8.345, 1.255, 123.456, 0.000001, 0.1, 9.995, 99.5,
  5e-324, 2.2250738585072014e-308, 1.7976931348623157e308, 999999999999999900000, 1e20, 4.35, 1e-7, 1e23,
  Math.pow(2, -60), Math.pow(2, 70), 9007199254740993, 0.000035, 1e-5, 1e15 + 0.5);
function fnv(h, s) { for (var i = 0; i < s.length; i++) h = Math.imul(h ^ s.charCodeAt(i), 16777619); return h >>> 0; }
var hf = 2166136261, he = 2166136261, hp = 2166136261, n = 0;
for (var v = 0; v < values.length; v++) {
  for (var sgn = 0; sgn < 2; sgn++) {
    var x = sgn ? -values[v] : values[v];
    for (var d = 0; d <= 100; d++) {
      if (Math.abs(x) < 1e21) hf = fnv(hf, x.toFixed(d));
      he = fnv(he, x.toExponential(d));
      if (d > 0) hp = fnv(hp, x.toPrecision(d));
      n++;
    }
  }
}
out.push("hashes " + n + " " + hf + " " + he + " " + hp);
var samples = [[2.5, 0], [1.005, 2], [8.345, 2], [96.44921875, 7], [0.000035, 5], [1e20 + 65536, 3], [1.25, 1],
  [5e-324, 3], [9.995, 2], [123.456, 20], [1e15 + 0.5, 0]];
for (var s = 0; s < samples.length; s++) {
  var y = samples[s][0], g = samples[s][1];
  out.push(y.toFixed(g) + " " + y.toExponential(g) + " " + y.toPrecision(g + 1) + " " + (-y).toPrecision(g + 2));
}
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
fn number_formatting_matches_node_at_every_digit_count() {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    assert_eq!(lines, node_output(SOURCE));
}
