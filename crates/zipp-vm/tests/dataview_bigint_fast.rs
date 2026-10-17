//! The interpreter's direct DataView BigInt accessors: both setters in both
//! byte orders over values across and beyond the 64-bit ranges, read back by
//! both getters, and everything that must still take the general path or
//! throw as it does (values ToBigInt coerces or refuses, a BigInt past the
//! i128 range, bad offsets, detached and length-tracking buffers, replaced,
//! shadowed and subclassed accessors). The expected lines are Node's.

const SOURCE: &str = r##"
var out = [];
var log = function () { out.push(Array.prototype.join.call(arguments, " ")); };
function attempt(label, f) {
  try { log(label, "ok", String(f())); } catch (e) { log(label, "threw", e.constructor.name); }
}
var buf = new ArrayBuffer(64);
var dv = new DataView(buf, 8, 40);
var u8 = new Uint8Array(buf);
var values = [0n, 1n, -1n, 127n, 255n, 256n, 2n ** 31n, 2n ** 32n, 2n ** 53n + 1n, 2n ** 63n - 1n, -(2n ** 63n),
  2n ** 63n, 2n ** 64n - 1n, 2n ** 64n, 2n ** 64n + 5n, -(2n ** 64n) - 3n, 2n ** 100n + 7n, -(2n ** 127n), 2n ** 200n + 9n];
var row = [];
for (var v = 0; v < values.length; v++) {
  for (var le = 0; le < 2; le++) {
    for (var s = 0; s < 2; s++) {
      u8.fill(0);
      if (s === 0) dv.setBigInt64(5, values[v], le === 1); else dv.setBigUint64(5, values[v], le === 1);
      row.push(String(dv.getBigInt64(5, le === 1)), String(dv.getBigUint64(5, le === 1)), String(dv.getBigInt64(5, le !== 1)));
      row.push(Array.prototype.join.call(u8.subarray(13, 21), "."));
    }
  }
}
var h = 0, all = row.join("|");
for (var i = 0; i < all.length; i++) h = Math.imul(h ^ all.charCodeAt(i), 16777619);
log("matrix", row.length, h >>> 0, row.slice(0, 4).join(" "));
log("mixed with numeric", (dv.setBigUint64(0, 0x0102030405060708n), dv.getUint32(0) + "," + dv.getUint32(4) + "," + dv.getUint8(7)));
attempt("number value", function () { return dv.setBigInt64(0, 5); });
attempt("string value", function () { dv.setBigInt64(0, "123"); return dv.getBigInt64(0); });
attempt("bad string", function () { return dv.setBigInt64(0, "1.5"); });
attempt("boolean value", function () { dv.setBigUint64(0, true); return dv.getBigUint64(0); });
var calls = [];
attempt("object value", function () { dv.setBigInt64(0, { valueOf: function () { calls.push("v"); return 77n; } }); return dv.getBigInt64(0) + calls.join(""); });
attempt("symbol value", function () { return dv.setBigInt64(0, Symbol()); });
attempt("past end", function () { return dv.setBigInt64(33, 1n); });
attempt("at end", function () { dv.setBigInt64(32, 9n); return dv.getBigInt64(32); });
attempt("get past end", function () { return dv.getBigUint64(33); });
attempt("negative offset", function () { return dv.getBigInt64(-1); });
attempt("fraction offset", function () { dv.setBigInt64(1.7, 3n); return dv.getBigInt64(1); });
attempt("string offset", function () { dv.setBigUint64("2", 4n); return dv.getBigUint64("2"); });
attempt("valueOf before bounds", function () { return dv.setBigInt64(1000, { valueOf: function () { throw new SyntaxError("first"); } }); });
attempt("default order", function () { dv.setBigUint64(0, 1n); return u8[8] + "," + u8[15]; });
var rab = new ArrayBuffer(16, { maxByteLength: 64 });
var tracking = new DataView(rab, 4);
attempt("tracking", function () { tracking.setBigInt64(4, -2n, true); rab.resize(40); tracking.setBigUint64(24, 3n); return tracking.getBigInt64(4, true) + ":" + tracking.getBigUint64(24); });
attempt("tracking shrunk", function () { rab.resize(10); return tracking.getBigInt64(0); });
var gone = new ArrayBuffer(16);
var gdv = new DataView(gone);
attempt("detached get", function () { gone.transfer(); return gdv.getBigInt64(0); });
attempt("detached set", function () { return gdv.setBigInt64(0, 1n); });
var own = new DataView(new ArrayBuffer(16));
own.getBigInt64 = function () { return "own"; };
log("shadowed", own.getBigInt64(0));
class MyView extends DataView { setBigUint64(p, v) { return "sub " + p + v; } }
log("subclass", new MyView(new ArrayBuffer(16)).setBigUint64(1, 2n));
var saved = DataView.prototype.getBigUint64;
DataView.prototype.getBigUint64 = function (p) { return "patched " + p; };
log("patched", dv.getBigUint64(0), new DataView(buf).getBigUint64(3));
DataView.prototype.getBigUint64 = saved;
log("restored", String(dv.getBigUint64(0)));
attempt("not a view", function () { return DataView.prototype.getBigInt64.call({}, 0); });
var hot = new DataView(new ArrayBuffer(4096)), acc = 0n;
for (var i = 0; i < 2000; i++) { hot.setBigInt64((i & 511) << 3, BigInt(i) * 1000003n - 77n, (i & 1) === 0); acc ^= hot.getBigInt64((i & 511) << 3, (i & 1) === 0); }
log("hot loop", String(acc));
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
fn dataview_bigint_accessors_match_node() {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    assert_eq!(lines, node_output(SOURCE));
}
