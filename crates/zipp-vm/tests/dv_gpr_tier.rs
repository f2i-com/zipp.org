//! W9: DataView `get*` on the INT-GPR tier — pinned receiver, the load landing
//! DIRECTLY in an i64 GPR home (no cvttsd2si pos round-trip, no cvtsi2sd
//! landing), fused-Eq / Bool-home / absent endian handling, deopt to the
//! interpreter for every miss.
//!
//! The kernels are FUNCTION-scoped: top-level vars are globals and every
//! global pins a permanent GPR home, which overflows the small pool and the
//! region correctly falls back to the DOUBLE tier (the typedarray-math bench's
//! own swizzle regions do exactly that — 13 homes against 7-9 GPRs — which is
//! why this wave's row-level prize is recorded as blocked on register
//! pressure, not on the arms). Fn-scoped locals share homes and the GPR tier
//! engages — `dv_gpr_micro` measured 1.10 ns/iter against the DOUBLE tier's
//! 3.47 and node's 2.86 on the fused getUint32 shape.
//!
//! Same harness as `dv_double_tier.rs`: every `dvg_parity_*` case asserts
//! byte-identical output against `node -e`, then the final test re-runs the
//! set under `ZIPP_NO_DV_GPR=1` (falls back to the DOUBLE arm),
//! `ZIPP_NO_GPR_HOMES=1` (xmm INT emitter — the DV retry is moot),
//! `ZIPP_NOJIT=1` and `ZIPP_JIT_THRESHOLD=1`, each in a child process.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output
}

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

/// Shared prelude: a 4KB buffer with high bytes set (negative-sign paths) and
/// a DataView over it.
const PRELUDE: &str = r#"
    "use strict";
    var buf = new ArrayBuffer(4096);
    var u8 = new Uint8Array(buf);
    for (var i = 0; i < 4096; i++) u8[i] = (i * 151 + 0x83) & 255;
    var dv = new DataView(buf);
"#;

/// The fused-Eq getUint32 shape (the micro that measured 1.10 ns/iter):
/// alternating endianness, `|0` accumulation.
#[test]
fn dvg_parity_fused_getuint32() {
    assert_matches_node(&format!(
        r#"{PRELUDE}
        function scan(dv, n) {{
          var s = 0;
          for (var i = 0; i < n; i++) {{
            var le = i & 1;
            s = (s + dv.getUint32((i & 1023) << 2, le === 1)) | 0;
          }}
          return s;
        }}
        console.log(scan(dv, 100000));
        "#
    ));
}

/// Every int-lane kind, signed kinds over sign-set data, LE via fused Eq, BE
/// via the absent flag, plus an argc==1 getInt8 — one kernel per kind so each
/// gets its own region and its own arms.
#[test]
fn dvg_parity_all_int_kinds() {
    assert_matches_node(&format!(
        r#"{PRELUDE}
        function k0(dv, n) {{ var s = 0; for (var i = 0; i < n; i++) s = (s + dv.getInt8(i & 4095)) | 0; return s; }}
        function k1(dv, n) {{ var s = 0; for (var i = 0; i < n; i++) s = (s + dv.getUint8(i & 4095)) | 0; return s; }}
        function k3(dv, n) {{ var s = 0; for (var i = 0; i < n; i++) {{ var le = i & 1; s = (s + dv.getInt16((i & 2047) << 1, le === 1)) | 0; }} return s; }}
        function k4(dv, n) {{ var s = 0; for (var i = 0; i < n; i++) s = (s + dv.getUint16((i & 2047) << 1)) | 0; return s; }}
        function k5(dv, n) {{ var s = 0; for (var i = 0; i < n; i++) {{ var le = i & 1; s = (s + dv.getInt32((i & 1023) << 2, le === 0)) | 0; }} return s; }}
        var N = 50000;
        console.log([k0(dv,N), k1(dv,N), k3(dv,N), k4(dv,N), k5(dv,N)].join(","));
        "#
    ));
}

/// A getUint32 above i32::MAX kept un-truncated: the home is WIDE (never
/// lazy — the census hazard this wave closed), and region exit boxes it as an
/// exact double. `+=` keeps the accumulator a double.
#[test]
fn dvg_parity_uint32_double_exit() {
    assert_matches_node(&format!(
        r#"{PRELUDE}
        function scan(dv, n) {{
          var s = 0;
          for (var i = 0; i < n; i++) s += dv.getUint32((i & 1023) << 2, true);
          return s;
        }}
        console.log(scan(dv, 100000));
        console.log(typeof scan(dv, 3));
        "#
    ));
}

/// A getUint32 result flowing through Bitwise on the SAME home a lazy pass
/// would target: `>>>`/`&` read the low 32 bits (right under either
/// representation), while the un-ORed copy must stay a u32.
#[test]
fn dvg_parity_uint32_through_bitwise() {
    assert_matches_node(&format!(
        r#"{PRELUDE}
        function scan(dv, n) {{
          var s = 0, hi = 0;
          for (var i = 0; i < n; i++) {{
            var v = dv.getUint32((i & 1023) << 2, (i & 1) === 1);
            s = (s + (v >>> 24) + (v & 255)) | 0;
            if (v > 0x7FFFFFFF) hi++;
          }}
          return s + ":" + hi;
        }}
        console.log(scan(dv, 100000));
        "#
    ));
}

/// The endian flag as a real Bool-typed register (not fused: the flag has
/// another use, so the Eq def survives into a Bool GPR home and the arm's
/// `test` is ToBoolean).
#[test]
fn dvg_parity_explicit_bool_flag() {
    assert_matches_node(&format!(
        r#"{PRELUDE}
        function scan(dv, n) {{
          var s = 0;
          for (var i = 0; i < n; i++) {{
            var f = (i & 3) === 0;
            var b = f ? 1 : 0;
            s = (s + dv.getUint16((i & 2047) << 1, f) + b) | 0;
          }}
          return s;
        }}
        console.log(scan(dv, 100000));
        "#
    ));
}

/// Boundary and miss semantics: pos == byteLength - size reads fine; the OOB
/// read after the loop went hot deopts and the interpreter raises RangeError.
#[test]
fn dvg_parity_boundary_and_rangeerror() {
    assert_matches_node(&format!(
        r#"{PRELUDE}
        function scan(dv, n) {{
          var s = 0;
          for (var i = 0; i < n; i++) {{
            var o = i & 4095;
            if (o > 4092) o = 4092;
            s = (s + dv.getUint32(o, true)) | 0;
          }}
          return s;
        }}
        console.log(scan(dv, 100000));
        var err = "";
        try {{ dv.getUint32(4093, true); }} catch (e) {{ err = e.constructor.name; }}
        console.log(err);
        "#
    ));
}

/// Detach mid-run: the loop goes hot on a region, then the buffer transfers
/// away — the snapshot re-take yields {0,0,0}, the identity guard misses, and
/// the interpreter raises the spec TypeError.
#[test]
fn dvg_parity_detach_typeerror() {
    assert_matches_node(&format!(
        r#"{PRELUDE}
        function scan(dv, n) {{
          var s = 0;
          for (var i = 0; i < n; i++) s = (s + dv.getUint8(i & 4095)) | 0;
          return s;
        }}
        console.log(scan(dv, 60000));
        buf.transfer();
        var err = "";
        try {{ scan(dv, 8); }} catch (e) {{ err = e.constructor.name; }}
        console.log(err);
        "#
    ));
}

/// Re-run every `dvg_parity_` case in four more modes, each in its own child
/// process (the env latches are read once per process). The same node-derived
/// assertions passing in all five modes is the parity check.
#[test]
fn all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    for (key, val) in [
        ("ZIPP_NO_DV_GPR", "1"),
        ("ZIPP_NO_GPR_HOMES", "1"),
        ("ZIPP_NOJIT", "1"),
        ("ZIPP_JIT_THRESHOLD", "1"),
    ] {
        let out = std::process::Command::new(&exe)
            .arg("dvg_parity_")
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
            "the dvg_parity_ filter matched nothing under {key}={val}:\n{stdout}"
        );
    }
}
