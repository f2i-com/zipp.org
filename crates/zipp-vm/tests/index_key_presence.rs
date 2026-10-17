//! `n in o`, `o.hasOwnProperty(n)` and `Object.hasOwn(o, n)` with an
//! array-index NUMBER on a plain object are answered by number, from the
//! same levels a read uses. Every case must match the string spelling and
//! Node: own, inherited and absent index keys, the null-prototype end, an
//! index on Object.prototype (data and accessor), an Array, a typed array
//! and a proxy in the chain, a class instance, integral doubles, negative,
//! fractional and out-of-range numbers, deleted keys of a large dictionary
//! (holes), and a dictionary churned across rounds. The expected lines are
//! Node's.

const SOURCE: &str = r##"
var out = [];
var log = function () { out.push(Array.prototype.join.call(arguments, " ")); };
var hasOwn = Object.prototype.hasOwnProperty;
function probe(label, o, keys) {
  log(label, keys.map(function (k) { return [k in o, hasOwn.call(o, k), Object.hasOwn(o, k), Reflect.has(o, k), String(k) in o].map(Number).join(""); }).join(" "));
}
var keys = [0, 1, 2, 5, 7, 1.0, 2.5, -1, -0, 4294967294, 4294967295, 1e21, NaN];
var o = { 0: "a", 2: "c", 5: "f", x: 1 };
probe("own", o, keys);
var child = Object.create(o); child[7] = "g";
probe("inherited", child, keys);
var bare = Object.create(null); bare[1] = 1;
probe("null proto", bare, keys);
Object.defineProperty(Object.prototype, 5, { value: "proto5", configurable: true });
probe("Object.prototype data", {}, keys);
delete Object.prototype[5];
Object.defineProperty(Object.prototype, 7, { get: function () { return "g"; }, configurable: true });
probe("Object.prototype accessor", { 1: 1 }, keys);
delete Object.prototype[7];
probe("restored", {}, keys);
probe("array proto", Object.create([10, 20, 30]), keys);
probe("typed proto", Object.create(new Uint8Array(4)), keys);
probe("proxy proto", Object.create(new Proxy({ 2: 1 }, {})), keys);
class C { constructor() { this[1] = 1; } }
probe("class instance", new C(), keys);
var big = {};
for (var i = 0; i < 200; i++) big[i * 3] = i;
for (var i = 0; i < 200; i += 2) delete big[i * 3];
probe("holey", big, [0, 3, 6, 9, 597, 594, 600, 1]);
var count = 0;
for (var r = 0; r < 5; r++) {
  for (var i = 0; i < 100; i++) big[i * 7 + r] = r;
  for (var i = 0; i < 100; i += 3) delete big[i * 7 + r];
  for (var i = 0; i < 800; i++) { if (i in big) count++; if (hasOwn.call(big, i)) count += 2; }
}
log("churn", count, Object.keys(big).length);
var sum = 0, hits = 0;
for (var i = 0; i < 50; i++) { if ((i * 7) in o) hits++; if (Object.hasOwn(child, i)) hits += 10; sum += o[i] === undefined ? 0 : 1; }
log("loop", hits, sum);
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
fn index_key_presence_matches_node() {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    assert_eq!(lines, node_output(SOURCE));
}
