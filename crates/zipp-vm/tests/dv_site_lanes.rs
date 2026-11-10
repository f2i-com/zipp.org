//! The interpreter's sites named for a numeric DataView accessor
//! (`Instr::GetPropDv`, `Instr::CallMethodDv`, `Instr::CallWithThisDv`, marked
//! by `bytecode::overlay_view_calls`) against Node: a pristine DataView's read
//! or write takes its lane by the accessor's prototype entry, and everything
//! else is what the unmarked instruction does. Every accessor at both byte
//! orders through a local receiver (the fused call) and in a script's loop over
//! globals (the accessor read, the arguments, then the call); receivers that
//! are not DataViews at such sites (a class with a `getUint8` method, an own
//! function, a getter, null, a primitive, a Proxy, a typed array); an own
//! shadow, the prototype's accessor replaced and put back, another prototype,
//! subclasses; positions, flags and values the lanes do not take (fractions,
//! strings, objects with `valueOf`, BigInts, out of range) and the order their
//! conversions run in; a resizable buffer under a length-tracking and a fixed
//! window, and a buffer transferred away; an argument that replaces the
//! accessor after it was read; a script's loop during which the prototype's
//! accessors change; `with`, `bind`, `call`, a computed name, a spread; and
//! last the prototype's entry made a getter, deleted and added back. In the
//! interpreter, by default and with every function compiled at once.

const SOURCE: &str = r##"var out = [];
function show(tag, v) { out.push(tag + " " + v); }
function name(f) { try { return String(f()); } catch (e) { return "throws " + e.constructor.name; } }

// 1. every numeric accessor, both byte orders, a local receiver (the fused call)
(function () {
  var buf = new ArrayBuffer(64);
  var dv = new DataView(buf, 8, 40);
  var u8 = new Uint8Array(buf);
  for (var i = 0; i < 64; i++) u8[i] = (i * 37 + 11) & 255;
  var acc = 0, facc = 0;
  for (var r = 0; r < 3000; r++) {
    var o = r % 30;
    var le = (r & 1) === 1;
    acc = (acc + dv.getInt8(o) + dv.getUint8(o + 1) + dv.getInt16(o, le) + dv.getUint16(o + 2, !le)) | 0;
    acc = (acc + dv.getInt32(o, le) + (dv.getUint32(o + 4, le) >>> 3)) | 0;
    var f = dv.getFloat32(o, le), d = dv.getFloat64(o, !le);
    if (isFinite(f)) facc += f % 1000;
    if (isFinite(d)) facc += d % 1000;
    dv.setInt8(o, r); dv.setUint8(o + 1, r * 3); dv.setInt16(o + 2, r * 257, le); dv.setUint16(o + 4, r * 513, !le);
    dv.setInt32(o + 6, r * 65537, le); dv.setUint32(o, r * 16777259, !le);
    if ((r & 7) === 0) { dv.setFloat32(o, r / 7, le); dv.setFloat64(o + 2, r / 13, !le); }
  }
  show("1 acc", acc); show("1 facc", facc.toFixed(3)); show("1 bytes", Array.prototype.join.call(u8, ","));
})();

// 2. a script's loop over globals (the split call: the accessor read, the arguments, the call)
var gbuf = new ArrayBuffer(32);
var gdv = new DataView(gbuf);
var gsum = 0;
for (var gi = 0; gi < 4000; gi++) {
  var go = gi & 15;
  gdv.setUint16(go, gi * 3, (gi & 1) === 0);
  gdv.setFloat32(go + 4, gi / 3, (gi & 2) === 0);
  gsum = (gsum + gdv.getUint32(go, (gi & 1) === 1) % 1000 + gdv.getInt8(go + 3) + gdv.getUint16(go + 1)) | 0;
}
show("2 gsum", gsum);
show("2 bytes", Array.prototype.join.call(new Uint8Array(gbuf), ","));

// 3. receivers that are not DataViews at sites named for an accessor
(function () {
  class Reader { constructor(n) { this.n = n; } getUint8(i) { return this.n + i; } setUint8(i, v) { this.n = v - i; return "set"; } }
  var plain = { getUint8: function (i) { return -i; }, setUint8: function (i, v) { this.last = v; } };
  var withGetter = { get getUint8() { return function (i) { return "g" + i; }; }, setUint8: function () {} };
  var dv = new DataView(new ArrayBuffer(8)); dv.setUint8(3, 99);
  var things = [dv, new Reader(5), plain, withGetter, new Reader(7)];
  var log = [];
  for (var i = 0; i < 2000; i++) {
    var t = things[i % 5];
    var v = t.getUint8(3);
    if (i < 10) log.push(v);
    if (i % 5 < 3) t.setUint8(3, i & 255);
  }
  show("3 log", log.join("|")); show("3 dv", dv.getUint8(3)); show("3 reader", things[1].n); show("3 plain", plain.last);
  show("3 null", name(function () { return null.getUint8(0); }));
  show("3 undef", name(function () { var u; return u.getUint8(0); }));
  show("3 number", name(function () { return (5).getUint8(0); }));
  show("3 string", name(function () { return "abc".getInt16(0); }));
  show("3 proxy", name(function () { return new Proxy(dv, {}).getUint8(3); }));
  show("3 call", name(function () { return DataView.prototype.getUint8.call({}, 0); }));
  show("3 borrowed", name(function () { var o = { getUint8: DataView.prototype.getUint8 }; return o.getUint8(0); }));
  show("3 typed", name(function () { return new Uint8Array(4).getUint8(0); }));
  show("3 buffer", name(function () { return new ArrayBuffer(4).getUint8(0); }));
})();

// 4. a shadow on the view, the prototype's accessor replaced and restored, another prototype, subclasses
(function () {
  var dv = new DataView(new ArrayBuffer(8)); dv.setUint8(0, 7);
  function read(v) { return v.getUint8(0); }
  function write(v, x) { v.setUint8(0, x); return v.getUint8(0); }
  var log = [];
  for (var i = 0; i < 300; i++) { read(dv); write(dv, 7); }
  log.push(read(dv));
  var saved = DataView.prototype.getUint8;
  DataView.prototype.getUint8 = function (i) { return "patched" + saved.call(this, i); };
  log.push(read(dv));
  DataView.prototype.getUint8 = saved;
  log.push(read(dv));
  var savedSet = DataView.prototype.setUint8;
  DataView.prototype.setUint8 = function (p, x) { return savedSet.call(this, p, x + 1); };
  log.push(write(dv, 20));
  DataView.prototype.setUint8 = savedSet;
  log.push(write(dv, 30));
  var other = new DataView(new ArrayBuffer(8)); other.setUint8(0, 9);
  for (var k = 0; k < 300; k++) read(other);
  Object.setPrototypeOf(other, { getUint8: function () { return "reproto"; } });
  log.push(read(other));
  Object.setPrototypeOf(other, DataView.prototype);
  log.push(read(other));
  class MyView extends DataView { getUint8(i) { return super.getUint8(i) + 1000; } }
  var mv = new MyView(new ArrayBuffer(8)); mv.setUint8(0, 5);
  for (var j = 0; j < 300; j++) read(mv);
  log.push(read(mv));
  class Plain extends DataView {}
  var pv = new Plain(new ArrayBuffer(8)); pv.setUint8(0, 6);
  for (var m = 0; m < 300; m++) read(pv);
  log.push(read(pv));
  var shadowed = new DataView(new ArrayBuffer(8)); shadowed.setUint8(0, 4);
  for (var n = 0; n < 300; n++) read(shadowed);
  shadowed.getUint8 = function () { return "own"; };
  log.push(read(shadowed));
  delete shadowed.getUint8;
  log.push(read(shadowed));
  shadowed.extra = 1;
  log.push(read(shadowed));
  show("4", log.join("|"));
})();

// 5. errors and conversions: positions, flags and values that are not what the lanes take
(function () {
  var dv = new DataView(new ArrayBuffer(16), 4, 8);
  for (var i = 0; i < 8; i++) dv.setUint8(i, i + 1);
  function g8(p) { return dv.getUint8(p); }
  function g32(p, le) { return dv.getUint32(p, le); }
  function s16(p, v, le) { return dv.setUint16(p, v, le); }
  for (var w = 0; w < 300; w++) { g8(w & 7); g32(w & 3, true); s16(6, 0x0708, false); }
  var log = [], calls = [];
  function pair() { return g8(0) + "," + g8(1); }
  log.push(name(function () { return g8(8); }));
  log.push(name(function () { return g8(-1); }));
  log.push(name(function () { return g32(5, true); }));
  log.push(name(function () { return g32(4, true); }));
  log.push(g8(1.9)); log.push(g8("2")); log.push(g8(undefined)); log.push(g8(NaN)); log.push(g8(null)); log.push(g8(true)); log.push(g8(-0.5));
  log.push(name(function () { return g8(Infinity); }));
  log.push(name(function () { return g8(Math.pow(2, 53)); }));
  log.push(name(function () { return g8(1n); }));
  log.push(name(function () { return g8(Symbol()); }));
  log.push(name(function () { return dv.getUint8(); }));
  log.push(g8({ valueOf: function () { calls.push("pos"); return 3; } }));
  log.push(g32(0, { valueOf: function () { calls.push("le"); return 0; } }));
  log.push(g32(0, "")); log.push(g32(0, "x")); log.push(g32(0, 0)); log.push(g32(0, 1)); log.push(g32(0, NaN));
  log.push(g32(0, 0n)); log.push(g32(0, 1n)); log.push(g32(0, null)); log.push(g32(0, [])); log.push(g32(0, -0)); log.push(g32(0, 0.5));
  log.push(String(s16(0, 0x1234, true))); log.push(pair());
  s16(0, 0x1234); log.push(pair());
  s16(0, 70000.9); log.push(pair());
  s16(0, -1); log.push(pair());
  s16(0, "258"); log.push(pair());
  s16(0, NaN); log.push(pair());
  s16(0, { valueOf: function () { calls.push("val"); return 515; } }); log.push(pair());
  s16(0); log.push(pair());
  s16(0, 1e21); log.push(pair());
  s16(0, -Infinity); log.push(pair());
  log.push(name(function () { return s16(7, 1); }));
  log.push(name(function () { return s16(0, 1n); }));
  log.push(name(function () {
    return s16({ valueOf: function () { calls.push("p2"); return 9; } }, { valueOf: function () { calls.push("v2"); return 1; } });
  }));
  dv.setFloat32(0, 1.1); log.push(dv.getFloat32(0)); dv.setFloat64(0, -0); log.push(1 / dv.getFloat64(0));
  dv.setFloat32(0, 16777217); log.push(dv.getFloat32(0)); dv.setInt32(0, 2147483648); log.push(dv.getInt32(0));
  dv.setUint32(0, -1); log.push(dv.getUint32(0)); dv.setUint32(0, 4294967296 + 7, true); log.push(dv.getUint32(0, true));
  dv.setInt8(0, 200); log.push(dv.getInt8(0)); dv.setUint8(0, -200.7); log.push(dv.getUint8(0));
  dv.setFloat64(0, NaN); log.push(dv.getFloat64(0)); dv.setFloat32(0, 1e40); log.push(dv.getFloat32(0));
  dv.setInt16(0, 32768, true); log.push(dv.getInt16(0, true)); log.push(dv.getUint16(0, true)); log.push(dv.getInt16(0));
  show("5", log.join("|")); show("5 calls", calls.join(","));
})();

// 6. a resizable buffer under a length-tracking and a fixed window, and a buffer that is transferred away
(function () {
  var log = [];
  var rab = new ArrayBuffer(8, { maxByteLength: 32 });
  var tracking = new DataView(rab);
  var fixed = new DataView(rab, 2, 4);
  function rt(p) { return tracking.getUint8(p); }
  function rf(p) { return fixed.getUint8(p); }
  function wt(p, v) { tracking.setUint8(p, v); }
  for (var i = 0; i < 8; i++) wt(i, i + 10);
  for (var w = 0; w < 300; w++) { rt(w & 7); rf(w & 3); }
  log.push(rt(7)); log.push(rf(3));
  log.push(name(function () { return rt(8); }));
  rab.resize(16);
  log.push(rt(8)); wt(15, 77); log.push(rt(15)); log.push(tracking.byteLength);
  rab.resize(4);
  log.push(name(function () { return rt(4); })); log.push(rt(3));
  log.push(name(function () { return rf(0); }));
  log.push(name(function () { return fixed.setUint8(0, 1); }));
  rab.resize(8); log.push(rf(0));
  var ab = new ArrayBuffer(8);
  var dv = new DataView(ab); dv.setUint8(0, 5);
  function rd() { return dv.getUint8(0); }
  function wr(v) { dv.setUint8(0, v); return "wrote"; }
  for (var k = 0; k < 300; k++) { rd(); wr(k & 255); }
  var moved = ab.transfer();
  log.push(name(rd)); log.push(name(function () { return wr(1); }));
  log.push(new DataView(moved).getUint8(0));
  show("6", log.join("|"));
})();

// 7. the accessor is read before the arguments run
(function () {
  var log = [];
  var dv = new DataView(new ArrayBuffer(8)); dv.setUint8(0, 42);
  var saved = DataView.prototype.getUint8;
  function f(v) { return v.getUint8((DataView.prototype.getUint8 = function () { return "replaced"; }, 0)); }
  log.push(f(dv));
  log.push(dv.getUint8(0));
  DataView.prototype.getUint8 = saved;
  log.push(dv.getUint8(0));
  var n = 0;
  function h(v, k) { return v.getUint8(k()); }
  for (var i = 0; i < 300; i++) n += h(dv, function () { return 0; });
  log.push(n);
  function g(v) { return v.getUint8((v.getUint8 = function () { return "own"; }, 0)); }
  log.push(g(dv)); log.push(dv.getUint8(0)); delete dv.getUint8; log.push(dv.getUint8(0));
  var ab = new ArrayBuffer(8), dv2 = new DataView(ab);
  log.push(name(function () { return dv2.getUint8((ab.transfer(), 0)); }));
  var dv3 = new DataView(new ArrayBuffer(8)); dv3.setUint8(2, 11);
  var savedSet = DataView.prototype.setUint8;
  log.push(name(function () { return dv3.setUint8((DataView.prototype.setUint8 = function () { return "replaced"; }, 2), 12); }));
  log.push(dv3.getUint8(2));
  log.push(dv3.setUint8(2, 13)); log.push(dv3.getUint8(2));
  DataView.prototype.setUint8 = savedSet;
  log.push(String(dv3.setUint8(2, 14))); log.push(dv3.getUint8(2));
  show("7", log.join("|"));
})();

// 8. a script's loop while the prototype's accessors are replaced and put back
var hdv = new DataView(new ArrayBuffer(8));
var hsum = 0, hsavedGet = DataView.prototype.getUint8, hsavedSet = DataView.prototype.setUint8;
hdv.setUint8(0, 3);
for (var hi = 0; hi < 600; hi++) {
  if (hi === 300) DataView.prototype.getUint8 = function (p) { return 100; };
  if (hi === 450) DataView.prototype.getUint8 = hsavedGet;
  if (hi === 500) DataView.prototype.setUint8 = function (p, v) { return hsavedSet.call(this, p, v + 1); };
  if (hi === 550) DataView.prototype.setUint8 = hsavedSet;
  if (hi >= 480) hdv.setUint8(hi & 0, hi & 3);
  hsum += hdv.getUint8(hi & 0);
}
show("8 hsum", hsum);

// 9. other ways to reach an accessor
(function () {
  var dv = new DataView(new ArrayBuffer(8)); new Uint8Array(dv.buffer)[1] = 33;
  var r; with (dv) { r = getUint8(1); }
  show("9 with", r);
  var m = dv.getUint8; show("9 bare", name(function () { return m(1); }));
  var b = dv.getUint8.bind(dv); show("9 bound", b(1));
  show("9 apply", dv.getUint8.apply(dv, [1])); show("9 call", dv.getUint8.call(dv, 1));
  show("9 optional", dv?.getUint8?.(1)); show("9 computed", dv["getUint8"](1));
  show("9 name", dv.getUint8.name + "/" + dv.getUint8.length + "/" + dv.setFloat64.length);
  show("9 frozen", (Object.freeze(dv), dv.getUint8(1)));
  show("9 spread", dv.getUint8(...[1]));
})();

// 10. last: the prototype's entry made an accessor, a data property again, deleted and added back
(function () {
  var log = [];
  var dv = new DataView(new ArrayBuffer(8)); new Uint8Array(dv.buffer)[0] = 7;
  function read(v) { return v.getUint8(0); }
  for (var i = 0; i < 300; i++) read(dv);
  var saved = DataView.prototype.getUint8;
  Object.defineProperty(DataView.prototype, "getUint8", { get: function () { return function () { return "accessor"; }; }, configurable: true });
  log.push(read(dv));
  Object.defineProperty(DataView.prototype, "getUint8", { value: saved, writable: true, configurable: true, enumerable: false });
  log.push(read(dv));
  delete DataView.prototype.getUint8;
  log.push(name(function () { return read(dv); }));
  DataView.prototype.getUint8 = saved;
  log.push(read(dv));
  log.push(dv.getInt8(0)); log.push(dv.getUint16(0));
  delete DataView.prototype.setUint8;
  log.push(name(function () { return dv.setUint8(0, 3); }));
  log.push(name(function () { return dv.setInt8(0, 4); })); log.push(read(dv));
  show("10", log.join("|"));
})();

console.log(out.join("\n"));
"##;

const EXPECTED: &str = "1 acc 317766812\n1 facc 59912.297\n1 bytes 11,48,85,122,159,196,233,14,222,155,52,157,138,159,67,161,54,163,140,165,226,167,67,169,142,171,228,173,58,175,67,177,230,179,60,181,146,183,1,247,189,121,183,183,11,183,11,214,251,32,69,106,143,180,217,254,35,72,109,146,183,220,1,38\n2 gsum 79594908\n2 bytes 176,46,182,46,188,46,194,46,200,46,206,46,212,46,218,46,221,171,68,68,166,160,0,0,0,0,0,0,0,0,0,0\n3 log 99|8|-3|g3|10|0|1|-3|g3|10\n3 dv 203\n3 reader 201\n3 plain 205\n3 null throws TypeError\n3 undef throws TypeError\n3 number throws TypeError\n3 string throws TypeError\n3 proxy throws TypeError\n3 call throws TypeError\n3 borrowed throws TypeError\n3 typed throws TypeError\n3 buffer throws TypeError\n4 7|patched7|7|21|30|reproto|9|1005|6|own|4|4\n5 throws RangeError|throws RangeError|throws RangeError|134678021|2|3|1|1|1|2|1|throws RangeError|throws RangeError|throws TypeError|throws TypeError|1|4|67305985|16909060|67305985|16909060|67305985|16909060|16909060|67305985|16909060|67305985|16909060|67305985|undefined|52,18|18,52|17,112|255,255|1,2|0,0|2,3|0,0|0,0|0,0|throws RangeError|throws TypeError|throws RangeError|1.100000023841858|-Infinity|16777216|-2147483648|4294967295|7|-56|56|NaN|Infinity|-32768|32768|128\n5 calls pos,val,p2,v2\n6 17|15|throws RangeError|0|77|16|throws RangeError|13|throws TypeError|throws TypeError|12|throws TypeError|throws TypeError|43\n7 42|replaced|42|12600|42|own|42|throws TypeError|undefined|12|replaced|12|undefined|14\n8 hsum 16220\n9 with 33\n9 bare throws TypeError\n9 bound 33\n9 apply 33\n9 call 33\n9 optional 33\n9 computed 33\n9 name getUint8/1/2\n9 frozen 33\n9 spread 33\n10 accessor|7|throws TypeError|7|7|1792|throws TypeError|undefined|4";

#[test]
fn dv_site_lanes_worker() {
    if std::env::var_os("ZIPP_DV_SITE_LANES_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n"), EXPECTED);
}

#[test]
fn dv_site_lanes_match_node() {
    if std::env::var_os("ZIPP_DV_SITE_LANES_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    for (mode, var) in [
        ("default", None),
        ("interpreter", Some("ZIPP_NOJIT")),
        ("compile-at-once", Some("ZIPP_JIT_THRESHOLD")),
    ] {
        let mut command = std::process::Command::new(&exe);
        command
            .args(["dv_site_lanes_worker", "--exact", "--nocapture"])
            .env("ZIPP_DV_SITE_LANES_WORKER", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD");
        if let Some(v) = var {
            command.env(v, "1");
        }
        let out = command.output().expect("spawn the worker");
        assert!(
            out.status.success(),
            "{mode} diverged from node:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
