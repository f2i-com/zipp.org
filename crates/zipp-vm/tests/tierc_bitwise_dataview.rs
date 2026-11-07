//! Tier C's bitwise slow path (`jit_bitwise_slow`) and DataView accessor lane
//! (`jit_dv_method_call`). The functions below run many times so Tier C
//! compiles them whole. Then they meet BigInt operands, NaN/±Infinity and
//! huge doubles, mixed BigInt/Number operands and `>>>` on BigInts (both
//! TypeErrors), objects with `valueOf`, and DataView calls on views that are
//! pristine, shadowed, re-prototyped, detached, out of bounds, or given
//! arguments that need coercion. The expected lines are Node's.

const SOURCE: &str = r##"
"use strict";
const out = [];
const log = (...a) => out.push(a.join(" "));
const t = (f) => { try { return f(); } catch (e) { return e.constructor.name; } };

function bits(a, b) {
  let h = 0;
  for (let i = 0; i < 4; i++) {
    const x = a & b, y = a | b, z = a ^ b, l = a << b, r = a >> b;
    h = (h * 31 + Number(BigInt.asIntN(32, BigInt(typeof x === "bigint" ? x : BigInt(x | 0)))) + Number(typeof y === "bigint" ? y & 0xffffn : y) + Number(typeof z === "bigint" ? z & 0xffn : z)) | 0;
    h = (h + Number(typeof l === "bigint" ? l & 0xffffn : l) + Number(typeof r === "bigint" ? r & 0xffffn : r)) | 0;
  }
  return h;
}
function ushr(a, b) { let h = 0; for (let i = 0; i < 3; i++) h = (h + (a >>> b)) % 1000003; return h; }
const NUMS = [0, 1, -1, 7, 2 ** 31, -(2 ** 31), 2 ** 32 + 5, 2 ** 53, 1e21, -1e21, 2 ** 63, -(2 ** 64), 3.7, -3.7, NaN, Infinity, -Infinity, -0];
const BIGS = [0n, 1n, -1n, 0xffffn, 2n ** 64n - 1n, -(2n ** 70n), 123456789012345678901234567890n, 5n];
let warm = 0;
for (let i = 0; i < 3000; i++) warm = (warm + bits(i, i & 7) + ushr(i, i & 31)) | 0;
log("warm", warm);
log("nums", NUMS.map((a) => NUMS.map((b) => bits(a, b & 31)).join(",")).join(";"));
log("ushr", NUMS.map((a) => NUMS.map((b) => ushr(a, b)).join(",")).join(";"));
log("bigs", BIGS.map((a) => BIGS.map((b) => t(() => bits(a, b < 0n ? -b % 64n : b % 64n))).join(",")).join(";"));
log("mixed", t(() => bits(1n, 1)), t(() => bits(1, 1n)), t(() => ushr(1n, 1n)), t(() => bits(1n, 100000000000n)));
let calls = 0;
const vo = { valueOf() { calls++; return 6; } };
log("valueOf", bits(vo, 2), calls, t(() => bits(Symbol.iterator, 1)), bits("12", "3"), bits(true, null), bits(undefined, 2));

// DataView accessors in a whole-function compiled body.
function rw(dv, n, le) {
  let h = 0;
  for (let i = 0; i + 16 <= n; i += 16) {
    dv.setUint32(i, i * 2654435761 >>> 0, le);
    dv.setInt16(i + 4, i - 3000, !le);
    dv.setFloat64(i + 8, i / 7, le);
    dv.setBigInt64(i, BigInt(i) * -1234567n, le);
    h = (h + dv.getUint8(i + 1) + dv.getInt16(i + 4, !le) + dv.getInt32(i, le) + (dv.getFloat64(i + 8, le) * 7 | 0)) | 0;
    h = (h + Number(dv.getBigInt64(i, le) & 0xffffn) + Number(dv.getBigUint64(i, !le) >> 50n)) | 0;
    h = (h + dv.getUint32(i + 4) + dv.getFloat32(i + 8, true) * 0) | 0;
  }
  return h;
}
const buf = new ArrayBuffer(4096);
const dv = new DataView(buf);
let w = 0;
for (let r = 0; r < 300; r++) w = (w + rw(dv, 4096, (r & 1) === 1)) | 0;
log("dv warm", w);
log("dv oob", t(() => rw(dv, 4112, true)), t(() => dv.getUint32(4094)));
// Coercing arguments: strings, doubles, objects, missing ones.
function coerce(dv, p, v) { dv.setUint16(p, v, true); return dv.getUint16(p, true) + "/" + dv.getInt8(p) + "/" + dv.getUint8(); }
log("coerce", coerce(dv, "2", 513), coerce(dv, 3.9, "65535"), coerce(dv, { valueOf() { return 6; } }, { valueOf() { return 258; } }), t(() => coerce(dv, -1, 1)));
// An own shadow, a replaced prototype method, a re-prototyped view.
const dv2 = new DataView(new ArrayBuffer(64));
for (let r = 0; r < 300; r++) rw(dv2, 64, false);
dv2.getUint8 = () => 99; log("own", rw(dv2, 64, false));
const orig = DataView.prototype.getInt32;
DataView.prototype.getInt32 = function () { return 1000; }; log("proto", rw(new DataView(new ArrayBuffer(64)), 64, true));
DataView.prototype.getInt32 = orig; log("restored", rw(new DataView(new ArrayBuffer(64)), 64, true));
const dv3 = new DataView(new ArrayBuffer(64)); Object.setPrototypeOf(dv3, { __proto__: DataView.prototype, setUint32() { return 0; } });
log("reproto", rw(dv3, 64, true));
// Detached and resized buffers.
const rb = new ArrayBuffer(64, { maxByteLength: 128 }); const dv4 = new DataView(rb);
log("resizable", rw(dv4, 64, true)); rb.resize(32); log("shrunk", t(() => rw(dv4, 64, true))); rb.resize(128); log("grown", rw(dv4, 64, true));
const db = new ArrayBuffer(64); const dv5 = new DataView(db); rw(dv5, 64, true); db.transfer(); log("detached", t(() => rw(dv5, 64, true)));
console.log(out.join("\n"));
"##;

fn node_output(src: &str) -> Vec<String> {
    let out = std::process::Command::new("node")
        .arg("-e")
        .arg(src)
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
fn tierc_bitwise_dataview_parity() {
    let got = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(got.error.is_none(), "unexpected runtime error: {:?}", got.error);
    let got: Vec<String> = got.output.iter().flat_map(|s| s.lines().map(str::to_owned)).collect();
    assert_eq!(got, node_output(SOURCE));
}

/// The corpus again with each new path switched off, eager JIT, GC stress and
/// the interpreter, each in a fresh process.
#[test]
fn tierc_bitwise_dataview_all_modes() {
    let exe = std::env::current_exe().expect("test exe path");
    for mode in [
        ("ZIPP_NO_TIERC_BITWISE_SLOW", "1"),
        ("ZIPP_NO_TIERC_DV_LANE", "1"),
        ("ZIPP_JIT_THRESHOLD", "1"),
        ("ZIPP_GC_STRESS", "1"),
        ("ZIPP_NOJIT", "1"),
    ] {
        let out = std::process::Command::new(&exe)
            .args(["tierc_bitwise_dataview_parity", "--exact"])
            .env(mode.0, mode.1)
            .output()
            .expect("spawn test binary");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{mode:?} failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!stdout.contains("running 0 tests"), "mode filter matched nothing: {stdout}");
    }
}
