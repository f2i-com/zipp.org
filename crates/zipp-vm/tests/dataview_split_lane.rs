//! DataView accessors called through the split member-call form (callee read before the
//! arguments) in compiled code, through the captured-method lane's DataView selector
//! (`captured_op::DV`, `jit_dv_method_call`): every numeric setter and getter at both
//! endiannesses and with non-boolean flags, Int and double values, BigInt accessors, an
//! override installed before the callee is read and one installed while the arguments
//! run, an own-property shadow, out-of-bounds writes and a coercing offset. Run
//! interpreted, at the default thresholds and with every function compiled at once; the
//! expected line is Node 24's.

const SOURCE: &str = r##"// DataView accessors called through the split member-call form, whose callee
// is read before the arguments (`dv.setUint32(off, o.v, le)`), in compiled
// code: every numeric setter and getter at both endiannesses and with
// non-boolean flags, Int and double values (modular, negative, NaN,
// fractional), BigInt accessors, an override installed before the callee
// is read (it runs) and one installed while the arguments run (the captured
// intrinsic runs), an own-property shadow, and the out-of-bounds RangeError.
"use strict";
const out = [];
const buf = new ArrayBuffer(64);
const u8 = new Uint8Array(buf);
function hash() { let h = 0; for (let i = 0; i < u8.length; i++) h = Math.imul(h ^ u8[i], 16777619); return h >>> 0; }
const vals = [0, 1, -1, 127, 128, 255, 256, -129, 65535, 65536, -32769, 2147483647, -2147483648, 4294967295, 4294967296,
  3.7, -3.7, 1e10, -1e10, NaN, Infinity, -Infinity, 0.5, -0, 1.5e-300];
const flags = [true, false, 1, 0, "", "x", null, undefined, {}];
function enc(dv, rec) {
  let off = 0;
  dv.setInt8(off, rec.v, rec.le); off += 1;
  dv.setUint8(off, rec.v, rec.le); off += 1;
  dv.setInt16(off, rec.v, rec.le); off += 2;
  dv.setUint16(off, rec.v, rec.le); off += 2;
  dv.setInt32(off, rec.v, rec.le); off += 4;
  dv.setUint32(off, rec.v, rec.le); off += 4;
  dv.setFloat32(off, rec.v, rec.le); off += 4;
  dv.setFloat64(off, rec.v, rec.le); off += 8;
  return dv.getInt8(0, rec.le) + "," + dv.getUint8(1, rec.le) + "," + dv.getInt16(2, rec.le) + "," + dv.getUint16(4, rec.le) + "," +
    dv.getInt32(6, rec.le) + "," + dv.getUint32(10, rec.le) + "," + dv.getFloat32(14, rec.le) + "," + dv.getFloat64(18, rec.le);
}
const dv = new DataView(buf, 3, 40);
let acc = 0, last = "";
for (let r = 0; r < 300; r++) {
  for (const v of vals) for (const le of flags) { last = enc(dv, { v, le }); acc = (acc + hash()) | 0; }
}
out.push(acc, last);
// BigInt accessors.
function big(dv, x, le) { dv.setBigInt64(0, x, le); dv.setBigUint64(8, x, !le); return String(dv.getBigInt64(0, le)) + "/" + String(dv.getBigUint64(8, !le)); }
let bs = "";
for (let r = 0; r < 300; r++) for (const x of [0n, 1n, -1n, (1n << 63n) - 1n, -(1n << 63n), 1n << 64n, 12345678901234567890n]) bs = big(dv, x, (r & 1) === 1);
out.push(bs, hash());
// An override installed while the arguments run: the captured intrinsic runs.
let swapped = 0;
const orig = DataView.prototype.setUint16;
function swapCall(dv, i) { const o = { get v() { if (i === 250) { DataView.prototype.setUint16 = function () { swapped++; }; } return i; } }; dv.setUint16(0, o.v, true); return dv.getUint16(0, true); }
let s1 = 0;
for (let i = 0; i < 300; i++) s1 += swapCall(dv, i);
out.push(s1, swapped);
DataView.prototype.setUint16 = orig;
// An override installed before the callee is read: it runs.
let calls = 0;
const origU32 = DataView.prototype.setUint32;
function plain(dv, i) { const o = { v: i }; dv.setUint32(4, o.v, false); return dv.getUint32(4, false); }
let s2 = 0;
for (let i = 0; i < 300; i++) { if (i === 200) DataView.prototype.setUint32 = function (p, v) { calls++; origU32.call(this, p, v + 1, false); }; s2 += plain(dv, i); }
DataView.prototype.setUint32 = origU32;
out.push(s2, calls);
// An own-property shadow on one view.
const dv2 = new DataView(buf);
let shadowCalls = 0;
for (let i = 0; i < 300; i++) { if (i === 150) dv2.setInt32 = function () { shadowCalls++; }; const o = { v: i }; dv2.setInt32(8, o.v, true); }
out.push(shadowCalls, dv2.getInt32(8, true));
// Out of bounds and a detached-free coercing offset.
let errs = [];
for (let i = 0; i < 300; i++) { try { const o = { v: i }; dv.setFloat64(36 + (i & 3), o.v, true); } catch (e) { if (errs.length < 3) errs.push(e.constructor.name + ":" + (36 + (i & 3))); } }
out.push(errs.join(";"));
let coerce = 0; const off = { valueOf() { coerce++; return 2; } };
for (let i = 0; i < 300; i++) { const o = { v: i }; dv.setUint8(off, o.v, true); }
out.push(coerce, dv.getUint8(2));
console.log(out.join("|"));
"##;

const EXPECTED: &str = "-342198252|0,0,0,0,0,0,0,1.5e-300|-6101065172474983726/12345678901234567890|585647805|43625|49|44950|100|150|149|RangeError:36;RangeError:37;RangeError:38|300|43";

#[test]
fn dataview_split_lane_worker() {
    if std::env::var_os("ZIPP_DATAVIEW_SPLIT_LANE_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n"), EXPECTED);
}

#[test]
fn dataview_split_lane_match_node() {
    if std::env::var_os("ZIPP_DATAVIEW_SPLIT_LANE_WORKER").is_some() {
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
            .args(["dataview_split_lane_worker", "--exact", "--nocapture"])
            .env("ZIPP_DATAVIEW_SPLIT_LANE_WORKER", "1")
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
