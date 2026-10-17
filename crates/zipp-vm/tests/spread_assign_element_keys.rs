//! Object spread and Object.assign copying whole stores that hold index
//! keys: the copies' key order in every enumeration, their lookups, their
//! independence from the source, a non-empty target, and the inherited
//! setters and read-only properties that must still see each Set. The
//! expected lines are Node's.

const SOURCE: &str = r##"
var out = [];
var log = function () { out.push(Array.prototype.join.call(arguments, " ")); };
function fnv(h, s) { for (var i = 0; i < s.length; i++) h = Math.imul(h ^ s.charCodeAt(i), 16777619); return h >>> 0; }
function digest(o) {
  var forin = [];
  for (var k in o) forin.push(k);
  return [Object.keys(o).length, fnv(0, Object.keys(o).join(",")), fnv(0, forin.join(",")),
    fnv(0, JSON.stringify(o)), fnv(0, JSON.stringify(Object.entries(o))), fnv(0, Object.values(o).join(","))].join(" ");
}
var syll = ["ka", "lo", "mi", "zu", "re", "pa"];
var d = {};
for (var i = 0; i < 600; i++) {
  var s = "", n = i;
  do { s += syll[n % 6]; n = (n / 6) | 0; } while (n > 0);
  d[s + "_" + (i & 3)] = i;
  if (i % 3 === 0) d[1 + ((i * 7919) % 100003)] = -i;
  if (i % 5 === 0) d[String(200000 + i)] = i * 2;
}
d["4294967295"] = "not an index"; d["01"] = "padded"; d["-1"] = "negative"; d[4294967294] = "last index";
for (var j = 0; j < 600; j += 4) delete d[1 + ((j * 7919) % 100003)];
var keys = Object.keys(d);
for (var j = 0; j < keys.length; j += 7) delete d[keys[j]];
var sourceDigest = digest(d);
log("source", sourceDigest);
var a = Object.assign({}, d);
log("assign", digest(a));
var b = { ...d };
log("spread", digest(b));
var c = { ...d, extra: 1, [keys[5]]: -5, 3: "three", 300000: "big" };
log("spread+", digest(c));
var e = Object.assign({ x: 1, 7: "seven", 200005: "kept?" }, d);
log("assign onto", digest(e));
log("lookups", a[1 + ((3 * 7919) % 100003)], b["200010"], a[4294967294], b["4294967295"], "01" in a, -1 in b, a[1] === d[1]);
a[1 + ((3 * 7919) % 100003)] = "changed"; delete b["200010"]; b[42] = "new";
log("independent", d[1 + ((3 * 7919) % 100003)], "200010" in d, 42 in d, digest(d) === sourceDigest);
log("after edits", digest(a), digest(b));
var small = { 2: "b", x: 1, 1: "a", y: 2, 0: "z", 10: "ten", 9: "nine" };
log("small", JSON.stringify({ ...small }), JSON.stringify(Object.assign({}, small)), Object.keys({ ...small, 5: "five", z: 3 }).join());
var calls = [];
Object.defineProperty(Object.prototype, "7", { set: function (v) { calls.push("set " + v); }, configurable: true });
var t = Object.assign({}, { 7: "via assign", q: 1 });
log("inherited setter", calls.join(), Object.keys(t).join(), JSON.stringify({ ...{ 7: "via spread" } }));
delete Object.prototype[7];
Object.defineProperty(Object.prototype, "8", { value: "ro", writable: false, configurable: true });
try { Object.assign({}, { 8: 1 }); log("read-only", "no throw"); } catch (err) { log("read-only", err.constructor.name); }
log("read-only spread", JSON.stringify({ ...{ 8: 1 } }));
delete Object.prototype[8];
var frozen = Object.freeze({ 3: "x" });
try { Object.assign(frozen, { 3: "y", 4: "z" }); log("frozen", "no throw"); } catch (err) { log("frozen", err.constructor.name, frozen[3], 4 in frozen); }
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
fn spread_and_assign_with_index_keys_match_node() {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    assert_eq!(lines, node_output(SOURCE));
}
