//! The interpreter's direct DataView setters: every numeric setter in both
//! byte orders over awkward values, and everything that must still take the
//! general path or throw as it does (coerced offsets and values, BigInts,
//! out-of-bounds and detached buffers, length-tracking views, a replaced,
//! shadowed or subclassed setter). The expected lines are Node's.

const SOURCE: &str = r##"
var out = [];
var log = function () { out.push(Array.prototype.join.call(arguments, " ")); };
function attempt(label, f) {
  try { log(label, "ok", String(f())); } catch (e) { log(label, "threw", e.constructor.name); }
}
var buf = new ArrayBuffer(64);
var dv = new DataView(buf, 8, 40);
var u8 = new Uint8Array(buf);
var setters = ["setInt8", "setUint8", "setInt16", "setUint16", "setInt32", "setUint32", "setFloat32", "setFloat64"];
var getters = ["getInt8", "getUint8", "getInt16", "getUint16", "getInt32", "getUint32", "getFloat32", "getFloat64"];
var values = [0, 1, -1, 127, 128, 255, 256, -129, 65535, 65536, 2147483647, -2147483648, 4294967295, 4294967296,
  1.5, -1.5, 0.1, -0, NaN, Infinity, -Infinity, 1e300, 3.4028235e38, 5e-324];
var row = [];
for (var s = 0; s < setters.length; s++) {
  for (var v = 0; v < values.length; v++) {
    for (var le = 0; le < 2; le++) {
      u8.fill(0);
      dv[setters[s]](3, values[v], le === 1);
      var got = dv[getters[s]](3, le === 1);
      row.push(Object.is(got, -0) ? "-0" : String(got));
      row.push(Array.prototype.join.call(u8.subarray(8, 20), "."));
    }
  }
}
var h = 0;
var all = row.join("|");
for (var i = 0; i < all.length; i++) h = Math.imul(h ^ all.charCodeAt(i), 16777619);
log("matrix", row.length, h >>> 0);
log("default order", (dv.setUint16(0, 0x1234), u8[8] + "," + u8[9]), (dv.setUint16(0, 0x1234, undefined), u8[8]), (dv.setUint16(0, 0x1234, "yes"), u8[8]));
attempt("past end", function () { return dv.setUint32(37, 1); });
attempt("at end", function () { return dv.setUint32(36, 7) === undefined && dv.getUint32(36); });
attempt("negative offset", function () { return dv.setUint8(-1, 1); });
attempt("fraction offset", function () { dv.setUint8(1.9, 9); return dv.getUint8(1); });
attempt("string offset", function () { dv.setUint8("2", 8); return dv.getUint8(2); });
attempt("string value", function () { dv.setInt16(0, "300"); return dv.getInt16(0); });
var calls = [];
attempt("object value", function () { dv.setInt32(0, { valueOf: function () { calls.push("v"); return 77; } }); return dv.getInt32(0) + calls.join(""); });
attempt("bigint value", function () { return dv.setInt32(0, 1n); });
attempt("valueOf before bounds", function () { return dv.setInt32(1000, { valueOf: function () { throw new SyntaxError("first"); } }); });
attempt("bigint setter", function () { dv.setBigInt64(0, -2n, true); return String(dv.getBigInt64(0, true)); });
var rab = new ArrayBuffer(16, { maxByteLength: 64 });
var tracking = new DataView(rab, 4);
attempt("tracking", function () { tracking.setUint32(8, 5); rab.resize(32); tracking.setUint32(24, 6); return tracking.getUint32(24) + tracking.byteLength; });
attempt("tracking shrunk", function () { rab.resize(8); return tracking.setUint32(0, 1); });
attempt("tracking out", function () { rab.resize(2); return tracking.setUint8(0, 1); });
var gone = new ArrayBuffer(8);
var gdv = new DataView(gone);
attempt("detached", function () { gone.transfer(); return gdv.setUint8(0, 1); });
var own = new DataView(new ArrayBuffer(8));
own.setUint8 = function () { return "own"; };
log("shadowed", own.setUint8(0, 1));
class MyView extends DataView { setUint8(p, v) { return "sub " + p + v; } }
log("subclass", new MyView(new ArrayBuffer(8)).setUint8(1, 2));
var saved = DataView.prototype.setFloat64;
DataView.prototype.setFloat64 = function (p, v) { return "patched " + v; };
log("patched", dv.setFloat64(0, 2.5), new DataView(buf).setFloat64(0, 3.5));
DataView.prototype.setFloat64 = saved;
log("restored", dv.setFloat64(0, 4.5), dv.getFloat64(0));
attempt("not a view", function () { return DataView.prototype.setUint8.call({}, 0, 1); });
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
fn dataview_setters_match_node() {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    assert_eq!(lines, node_output(SOURCE));
}
