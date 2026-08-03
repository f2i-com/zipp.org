//! DataView `get*` on the DOUBLE (regalloc) tier: pinned receiver, unboxed
//! f64-home results, inline endian handling, deopt-to-interpreter for every
//! miss (OOB pos → RangeError, detached/shrunk buffer → TypeError, fractional
//! pos → ToIndex truncation).
//!
//! Every `dv_parity_*` case asserts byte-identical output against `node -e`
//! (node v24 expected on PATH, the same precondition as `backedge_dense.rs`),
//! at DEFAULT thresholds — hot enough for the swizzle regions to OSR-compile
//! on the double tier. The final test re-runs the whole `dv_parity_` set in
//! three more modes, each in its own child process (the env latches are read
//! once per process): `ZIPP_NO_DV_DOUBLE=1` (the off-switch — helper-call MEM
//! tier), `ZIPP_NOJIT=1` (pure interpreter), and `ZIPP_JIT_THRESHOLD=1`
//! (compile everything immediately). The same node-derived assertions passing
//! in all four modes is the four-mode parity check.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output
}

/// The same program's output from `node -e`, so expectations aren't
/// hand-computed.
fn node_output(src: &str) -> Vec<String> {
    let out = std::process::Command::new("node")
        .arg("-e")
        .arg(src)
        .output()
        .expect("node v24 on PATH (expected values come from `node -e`)");
    assert!(out.status.success(), "node failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout)
        .expect("node output is UTF-8")
        .lines()
        .map(|l| l.to_string())
        .collect()
}

fn assert_matches_node(src: &str) {
    let ours = run_ok(src);
    let node = node_output(src);
    assert_eq!(ours, node, "zipp != node for: {src}");
}

/// The bench's swizzle kernel shape (bench/real/typedarray-math.js:35-43),
/// scaled down: top-level vars (globals), `le === 1` / `le === 0` flags, an
/// argc==1 getInt8, mixed strides — the exact shape the double tier now hosts.
#[test]
fn dv_parity_bench_swizzle_shape() {
    assert_matches_node(
        r#"
        "use strict";
        var NI = 4096;
        var iv = new Int32Array(NI);
        var st = 0x9E3779B9 | 0;
        for (var i = 0; i < NI; i++) {
          st ^= st << 13; st ^= st >>> 17; st ^= st << 5;
          iv[i] = st | 0;
        }
        var dv = new DataView(iv.buffer, 0, 1024 * 4);
        var bsum = 0;
        for (var r = 0; r < 60; r++) {
          for (var o = 0; o < 1024 * 4; o += 4) {
            var le = (o >> 2) & 1;
            var v = dv.getUint32(o, le === 1);
            bsum = (bsum + (v >>> 24) + (v & 255) + dv.getUint16(o, le === 0) + dv.getInt8(o + 2)) | 0;
          }
        }
        console.log("bsum=" + bsum);
        "#,
    );
}

/// All eight whitelisted kinds, both endiannesses, unaligned offsets. The int
/// kinds accumulate through `|0`; the float kinds read bytes WRITTEN AS
/// doubles/floats so the sums stay finite and bit-deterministic.
#[test]
fn dv_parity_all_kinds_both_endians_unaligned() {
    assert_matches_node(
        r#"
        "use strict";
        var b = new ArrayBuffer(256);
        var u8 = new Uint8Array(b);
        for (var i = 0; i < 256; i++) u8[i] = (i * 167 + 13) & 255;
        var dv = new DataView(b);
        var s = 0;
        for (var r = 0; r < 400; r++) {
          for (var o = 0; o < 32; o++) {
            var f = (o & 1);
            s = (s + dv.getInt8(o + 1)
                   + dv.getUint8(o)
                   + dv.getInt16(o + 1, f === 1)
                   + dv.getUint16(o + 3, f === 0)
                   + dv.getInt32(o + 1, f === 1)
                   + (dv.getUint32(o + 5, f === 0) >>> 3)) | 0;
          }
        }
        console.log("isum=" + s);
        var xf = new Float64Array(8);
        for (var i = 0; i < 8; i++) xf[i] = i * 1.5 - 3.25;
        var dvf = new DataView(xf.buffer);
        var fs = 0;
        for (var r = 0; r < 2000; r++) {
          for (var o = 0; o < 7; o++) {
            var f = (o & 1);
            fs += dvf.getFloat64(o * 8, f === 1) + dvf.getFloat64(o * 8, f === 0);
          }
        }
        console.log("fsum=" + fs.toFixed(4));
        var x4 = new Float32Array(8);
        for (var i = 0; i < 8; i++) x4[i] = i * 0.75 + 0.125;
        var dv4 = new DataView(x4.buffer);
        var f4 = 0;
        for (var r = 0; r < 2000; r++) {
          for (var o = 0; o < 7; o++) {
            var f = (o & 1);
            f4 += dv4.getFloat32(o * 4, f === 1) + dv4.getFloat32(o * 4, f === 0);
          }
        }
        console.log("f4sum=" + f4.toFixed(4));
        "#,
    );
}

/// getFloat64/getFloat32 over bytes that FORM NaN patterns aliasing the
/// engine's NaN-box tags. The inline path must canonicalise: the result is a
/// plain NaN number, `typeof` stays "number", and arithmetic propagates NaN —
/// a forged tag would answer differently (or crash).
#[test]
fn dv_parity_float_nan_payloads_stay_nan() {
    assert_matches_node(
        r#"
        "use strict";
        var b = new ArrayBuffer(64);
        var u8 = new Uint8Array(b);
        // Little-endian f64 bit patterns 0x7FF9_0000_0000_0001 (an Int-tag
        // alias), 0xFFF9_0000_0000_0002, 0x7FFD_0000_0000_0003 (a heap-tag
        // alias), and a signalling-f32 pattern 0x7FC80001 repeated.
        var pats = [
          [1,0,0,0,0,0,0xF9,0x7F],
          [2,0,0,0,0,0,0xF9,0xFF],
          [3,0,0,0,0,0,0xFD,0x7F],
          [1,0,0xC8,0x7F,1,0,0xC8,0x7F]
        ];
        for (var p = 0; p < 4; p++)
          for (var k = 0; k < 8; k++) u8[p * 8 + k] = pats[p][k];
        var dv = new DataView(b);
        var nan = 0, num = 0, sum = 0;
        for (var r = 0; r < 3000; r++) {
          for (var p = 0; p < 4; p++) {
            var v = dv.getFloat64(p * 8, (p & 1) === 0);
            var w = dv.getFloat32(p * 8 + 2, (p & 1) === 1);
            if (v !== v) nan++; else sum += v;
            if (typeof v === "number") num++;
            if (w !== w) nan++; else sum += w;
          }
        }
        var probe = dv.getFloat64(0, true) + 1;
        console.log(nan + " " + num + " " + (probe !== probe) + " " + (sum === sum));
        "#,
    );
}

/// Boundary positions: the last in-range pos for each size succeeds, one past
/// it throws RangeError, negative and huge positions throw, fractional and
/// NaN positions take ToIndex truncation — with the throwing access executed
/// FROM the hot compiled loop (guard → deopt → the interpreter throws).
#[test]
fn dv_parity_bounds_and_weird_positions() {
    assert_matches_node(
        r#"
        "use strict";
        var b = new ArrayBuffer(16);
        var u8 = new Uint8Array(b);
        for (var i = 0; i < 16; i++) u8[i] = (i * 37 + 11) & 255;
        var dv = new DataView(b, 3, 13); // byteOffset'd view, byteLength 13
        function t(f) { try { return "v:" + f(); } catch (e) { return "e:" + e.constructor.name; } }
        var s = 0;
        function hot(n, oob) {
          var acc = 0;
          for (var i = 0; i < n; i++) {
            var o = (i === n - 1 && oob) ? 10 : (i & 7);
            acc = (acc + dv.getUint32(o, (i & 1) === 1)) | 0;
          }
          return acc;
        }
        s = hot(4000, false);                    // warm + compile
        console.log("warm=" + s);
        console.log(t(function(){ return hot(50, true); })); // 10 > 13-4 → RangeError out of the region
        console.log(t(function(){ return dv.getUint32(9, true); }));  // last valid u32 pos
        console.log(t(function(){ return dv.getFloat64(5, true); })); // last valid f64 pos
        console.log(t(function(){ return dv.getFloat64(6, true); }));
        console.log(t(function(){ return dv.getUint16(11, true); }));
        console.log(t(function(){ return dv.getUint16(12, true); }));
        console.log(t(function(){ return dv.getUint8(12); }));
        console.log(t(function(){ return dv.getUint8(13); }));
        console.log(t(function(){ return dv.getInt8(-1); }));
        console.log(t(function(){ return dv.getUint32(2.5, true); }));  // ToIndex trunc → pos 2
        console.log(t(function(){ return dv.getUint32(NaN, true); }));  // ToIndex → 0
        console.log(t(function(){ return dv.getUint32(-0, true); }));   // → 0
        console.log(t(function(){ return dv.getUint32(Math.pow(2, 53), true); }));
        console.log(t(function(){ return dv.getUint16("2", false); })); // string pos → generic path
        "#,
    );
}

/// Endian-flag shapes beyond the fused `x === k` form: literal true/false,
/// numeric flags (ToBoolean via the generic path), an absent flag (argc 1 →
/// big-endian), and a flag computed from a fractional compare.
#[test]
fn dv_parity_flag_shapes() {
    assert_matches_node(
        r#"
        "use strict";
        var b = new ArrayBuffer(32);
        var u8 = new Uint8Array(b);
        for (var i = 0; i < 32; i++) u8[i] = (i * 91 + 7) & 255;
        var dv = new DataView(b);
        var s = 0;
        for (var r = 0; r < 3000; r++) {
          var o = r & 7;
          var half = o * 0.5;
          s = (s + dv.getUint32(o, true)
                 + dv.getUint32(o, false)
                 + dv.getUint32(o)
                 + dv.getUint32(o, 1)
                 + dv.getUint32(o, 0)
                 + dv.getUint16(o, half === 1)) | 0;
        }
        console.log("flags=" + s);
        "#,
    );
}

/// The receiver global reassigned INSIDE the loop (two views, different
/// byteOffsets): the store kills the pin, the region falls back, answers must
/// not change.
#[test]
fn dv_parity_receiver_swap_mid_loop() {
    assert_matches_node(
        r#"
        "use strict";
        var b = new ArrayBuffer(64);
        var u8 = new Uint8Array(b);
        for (var i = 0; i < 64; i++) u8[i] = (i * 53 + 29) & 255;
        var a1 = new DataView(b, 0, 32);
        var a2 = new DataView(b, 16, 32);
        var dv = a1;
        var s = 0;
        for (var r = 0; r < 6000; r++) {
          dv = ((r & 3) === 0) ? a2 : a1;
          s = (s + dv.getUint32(r & 15, (r & 1) === 1) + dv.getInt16(r & 15, (r & 1) === 0)) | 0;
        }
        console.log("swap=" + s);
        "#,
    );
}

/// Detach (transfer) between hot runs: the next entry's snapshot is all-zero,
/// the identity guard misses, and the interpreter throws the TypeError.
#[test]
fn dv_parity_detach_between_runs() {
    assert_matches_node(
        r#"
        "use strict";
        var b = new ArrayBuffer(64);
        var u8 = new Uint8Array(b);
        for (var i = 0; i < 64; i++) u8[i] = (i * 11 + 3) & 255;
        var dv = new DataView(b);
        function hot(n) {
          var acc = 0;
          for (var i = 0; i < n; i++) acc = (acc + dv.getUint32(i & 15, (i & 1) === 1)) | 0;
          return acc;
        }
        var before = hot(4000);
        b.transfer();
        var after;
        try { after = "v:" + hot(40); } catch (e) { after = "e:" + e.constructor.name; }
        console.log(before + " " + after);
        "#,
    );
}

/// A resizable buffer shrunk under the view mid-program: accesses that were in
/// range start throwing; growing it back restores them. (The region
/// re-snapshots at entry; an out-of-bounds view snapshots all-zero.)
#[test]
fn dv_parity_resizable_shrink_and_regrow() {
    assert_matches_node(
        r#"
        "use strict";
        var b = new ArrayBuffer(32, { maxByteLength: 64 });
        var u8 = new Uint8Array(b);
        for (var i = 0; i < 32; i++) u8[i] = (i * 19 + 5) & 255;
        var dv = new DataView(b, 8, 16);
        function hot(n) {
          var acc = 0;
          for (var i = 0; i < n; i++) acc = (acc + dv.getUint16(i & 7, (i & 1) === 0)) | 0;
          return acc;
        }
        var before = hot(4000);
        b.resize(12);
        var mid;
        try { mid = "v:" + hot(40); } catch (e) { mid = "e:" + e.constructor.name; }
        b.resize(32);
        var again;
        try { again = "v:" + hot(4000); } catch (e) { again = "e:" + e.constructor.name; }
        console.log(before + " " + mid + " " + again);
        "#,
    );
}

/// A same-named method on a NON-DataView receiver keeps calling the user
/// function — the pin never matches a plain object, and the call-IC path
/// resolves the method exactly as before.
#[test]
fn dv_parity_non_dataview_receiver_same_name() {
    assert_matches_node(
        r#"
        "use strict";
        var dv = { getUint32: function (o, le) { return o * 2 + (le ? 1 : 0); } };
        var s = 0;
        for (var r = 0; r < 3000; r++) {
          s = (s + dv.getUint32(r & 7, (r & 1) === 1)) | 0;
        }
        console.log("obj=" + s);
        "#,
    );
}

/// Re-run every `dv_parity_` case in three more modes, each in its own child
/// process (the env latches are read once per process): the off-switch, the
/// pure interpreter, and threshold-1. The same node-derived assertions
/// passing in all modes IS the four-mode parity check.
#[test]
fn all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    for (key, val) in [
        ("ZIPP_NO_DV_DOUBLE", "1"),
        ("ZIPP_NOJIT", "1"),
        ("ZIPP_JIT_THRESHOLD", "1"),
    ] {
        let out = std::process::Command::new(&exe)
            .arg("dv_parity_")
            .env(key, val)
            .output()
            .expect("spawn the test binary");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{key}={val} mode failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !stdout.contains("running 0 tests"),
            "the dv_parity_ filter matched nothing under {key}={val}:\n{stdout}"
        );
    }
}
