//! The interpreter's numeric typed-array store converts and writes each
//! element at its kind's width in one step (`ta_interp_numeric_set_fixed`;
//! `ZIPP_NO_TA_DIRECT_STORE` builds the bytes through `ta_encode` again):
//! every non-BigInt kind with edge Numbers (signed zero, the wrap and clamp
//! boundaries, fractions and their rounding, values past 2^53, 2^63 and
//! 2^64, NaN and the infinities, the half-precision overflow), stores past
//! the end and at a non-index key, views at an offset into a shared buffer
//! with indices past the view, and a hot store loop. The expected lines are
//! Node's (v24), with the direct store, without it and in the interpreter.

const SOURCE: &str = r##"
var kinds = [Int8Array, Uint8Array, Uint8ClampedArray, Int16Array, Uint16Array, Int32Array, Uint32Array,
  Float32Array, Float64Array];
if (typeof Float16Array === 'function') kinds.push(Float16Array);
var vals = [0, -0, 1, -1, 127, 128, 255, 256, -128, -129, 32767, 32768, 65535, 65536, -32769,
  2147483647, 2147483648, -2147483648, -2147483649, 4294967295, 4294967296, 4294967297,
  0.5, 1.5, 2.5, -0.5, -1.5, 254.5, 255.5, 3.7, -3.7, 1e10, -1e10, 1e20, -1e20, 9007199254740993,
  9.3e18, -9.3e18, 1.8446744073709552e19, 3e19, NaN, Infinity, -Infinity, 1e-300, 65504, 65520, 1/3];
var out = [];
for (var k = 0; k < kinds.length; k++) {
  var K = kinds[k];
  var a = new K(vals.length + 2);
  for (var r = 0; r < 3; r++) {
    for (var i = 0; i < vals.length; i++) a[i] = vals[i];
    a[vals.length + 5] = 7;
    a[-1] = 7;
  }
  var s = [];
  for (var i = 0; i < a.length; i++) s.push(Object.is(a[i], -0) ? '-0' : String(a[i]));
  out.push(K.name + ': ' + s.join(','));
}
var buf = new ArrayBuffer(32);
var v32 = new Int32Array(buf, 8, 4);
var f64 = new Float64Array(buf, 0, 4);
for (var i = 0; i < 6; i++) { v32[i] = i * 1000003 + 0.75; }
f64[3] = 1.25;
out.push('offset: ' + Array.from(new Uint8Array(buf)).join(','));
var sum = 0;
var big = new Float64Array(1024);
for (var i = 0; i < 20000; i++) { big[i & 1023] = big[(i + 1) & 1023] + 1; }
for (var i = 0; i < 1024; i++) sum += big[i];
out.push('loop: ' + sum);
console.log(out.join('\n'));
"##;

const EXPECTED: &str = "Int8Array: 0,0,1,-1,127,-128,-1,0,-128,127,-1,0,-1,0,-1,-1,0,0,-1,-1,0,1,0,1,2,0,-1,-2,-1,3,-3,0,0,0,0,0,0,0,0,0,0,0,0,0,-32,-16,0,0,0
Uint8Array: 0,0,1,255,127,128,255,0,128,127,255,0,255,0,255,255,0,0,255,255,0,1,0,1,2,0,255,254,255,3,253,0,0,0,0,0,0,0,0,0,0,0,0,0,224,240,0,0,0
Uint8ClampedArray: 0,0,1,0,127,128,255,255,0,0,255,255,255,255,0,255,255,0,0,255,255,255,0,2,2,0,0,254,255,4,0,255,0,255,0,255,255,0,255,255,0,255,0,0,255,255,0,0,0
Int16Array: 0,0,1,-1,127,128,255,256,-128,-129,32767,-32768,-1,0,32767,-1,0,0,-1,-1,0,1,0,1,2,0,-1,254,255,3,-3,-7168,7168,0,0,0,0,0,0,0,0,0,0,0,-32,-16,0,0,0
Uint16Array: 0,0,1,65535,127,128,255,256,65408,65407,32767,32768,65535,0,32767,65535,0,0,65535,65535,0,1,0,1,2,0,65535,254,255,3,65533,58368,7168,0,0,0,0,0,0,0,0,0,0,0,65504,65520,0,0,0
Int32Array: 0,0,1,-1,127,128,255,256,-128,-129,32767,32768,65535,65536,-32769,2147483647,-2147483648,-2147483648,2147483647,-1,0,1,0,1,2,0,-1,254,255,3,-3,1410065408,-1410065408,1661992960,-1661992960,0,-81657856,81657856,0,-1648885760,0,0,0,0,65504,65520,0,0,0
Uint32Array: 0,0,1,4294967295,127,128,255,256,4294967168,4294967167,32767,32768,65535,65536,4294934527,2147483647,2147483648,2147483648,2147483647,4294967295,0,1,0,1,2,0,4294967295,254,255,3,4294967293,1410065408,2884901888,1661992960,2632974336,0,4213309440,81657856,0,2646081536,0,0,0,0,65504,65520,0,0,0
Float32Array: 0,-0,1,-1,127,128,255,256,-128,-129,32767,32768,65535,65536,-32769,2147483648,2147483648,-2147483648,-2147483648,4294967296,4294967296,4294967296,0.5,1.5,2.5,-0.5,-1.5,254.5,255.5,3.700000047683716,-3.700000047683716,10000000000,-10000000000,100000002004087730000,-100000002004087730000,9007199254740992,9300000300729369000,-9300000300729369000,18446744073709552000,30000001041030970000,NaN,Infinity,-Infinity,0,65504,65520,0.3333333432674408,0,0
Float64Array: 0,-0,1,-1,127,128,255,256,-128,-129,32767,32768,65535,65536,-32769,2147483647,2147483648,-2147483648,-2147483649,4294967295,4294967296,4294967297,0.5,1.5,2.5,-0.5,-1.5,254.5,255.5,3.7,-3.7,10000000000,-10000000000,100000000000000000000,-100000000000000000000,9007199254740992,9300000000000000000,-9300000000000000000,18446744073709552000,30000000000000000000,NaN,Infinity,-Infinity,1e-300,65504,65520,0.3333333333333333,0,0
Float16Array: 0,-0,1,-1,127,128,255,256,-128,-129,32768,32768,Infinity,Infinity,-32768,Infinity,Infinity,-Infinity,-Infinity,Infinity,Infinity,Infinity,0.5,1.5,2.5,-0.5,-1.5,254.5,255.5,3.69921875,-3.69921875,Infinity,-Infinity,Infinity,-Infinity,Infinity,Infinity,-Infinity,Infinity,Infinity,NaN,Infinity,-Infinity,0,65504,Infinity,0.333251953125,0,0
offset: 0,0,0,0,0,0,0,0,0,0,0,0,67,66,15,0,134,132,30,0,201,198,45,0,0,0,0,0,0,0,244,63
loop: 20019";

#[test]
fn typed_array_direct_store_worker() {
    if std::env::var_os("ZIPP_TA_DIRECT_STORE_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n"), EXPECTED);
}

#[test]
fn typed_array_direct_store_matches_node() {
    if std::env::var_os("ZIPP_TA_DIRECT_STORE_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    for (mode, var) in [
        ("default", None),
        ("direct-store-off", Some("ZIPP_NO_TA_DIRECT_STORE")),
        ("interpreter", Some("ZIPP_NOJIT")),
    ] {
        let mut command = std::process::Command::new(&exe);
        command
            .args(["typed_array_direct_store_worker", "--exact", "--nocapture"])
            .env("ZIPP_TA_DIRECT_STORE_WORKER", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_NO_TA_DIRECT_STORE");
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
