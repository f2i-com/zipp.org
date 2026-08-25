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
//! W10.3 extends the tier with FRAME SPILL SLOTS: on the post-share-re-plan
//! attempt a pool overflow maps the coldest homes (weighted use census) to
//! `[rsp + disp]` qword slots instead of declining, so the `dvg_parity_spill_*`
//! kernels below — more live values than the pool — now engage with a few
//! spilled homes (slot = canonical sign-extended i64; spilled dsts stage
//! through rax/eax; flush/guard/entry paths read the slots).
//!
//! Same harness as `dv_double_tier.rs`: every `dvg_parity_*` case asserts
//! byte-identical output against `node -e`, then the final test re-runs the
//! set under `ZIPP_NO_DV_GPR=1` (falls back to the DOUBLE arm),
//! `ZIPP_NO_GPR_HOMES=1` (xmm INT emitter — the DV retry is moot),
//! `ZIPP_NO_GPR_SPILL_SLOTS=1` (W10.3 off — overflow declines as before),
//! `ZIPP_NOJIT=1` and `ZIPP_JIT_THRESHOLD=1`, each in a child process.

fn run_ok(src: &str) -> Vec<String> {
    // W10.3 is OPT-IN (dark — it measured slower than the DOUBLE incumbent on
    // its target row); this suite exists to exercise it, so latch it on for
    // every in-process engine. The all_modes children inherit it, and the
    // ZIPP_NO_GPR_SPILL_SLOTS=1 mode row still forces it off over the opt-in.
    static SPILL_ON: std::sync::Once = std::sync::Once::new();
    SPILL_ON.call_once(|| std::env::set_var("ZIPP_GPR_SPILL_SLOTS", "1"));
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

fn node_output(src: &str) -> Vec<String> {
    let out = std::process::Command::new("node")
        .arg("-e")
        .arg(src)
        .output()
        .expect("node v24 on PATH (expected values come from `node -e`)");
    assert!(
        out.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
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

/// ── W10.3 GPR spill slots ── a kernel with MORE simultaneously-live values
/// than the GPR pool (12 loop-carried accumulators + the counter + the DV
/// load) overflows on the first attempt, the shared-home re-plan cannot shrink
/// loop-carried homes, and the post-share attempt maps the coldest homes to
/// frame spill slots instead of declining to the DOUBLE tier. Spilled Add
/// dsts stage through rax (raw slot store, i53 guard loads from the slot) and
/// flush_exit boxes them from their slots.
#[test]
fn dvg_parity_spill_many_live() {
    assert_matches_node(&format!(
        r#"{PRELUDE}
        function scan(dv, n) {{
          var s0=0,s1=0,s2=0,s3=0,s4=0,s5=0,s6=0,s7=0;
          for (var i = 0; i < n; i++) {{
            var le = i & 1; var v = dv.getInt32((i & 1023) << 2, le === 1);
            s0=(s0+v)|0; s1=(s1+(v>>1))|0; s2=(s2+(v>>2))|0; s3=(s3+(v>>3))|0;
            s4=(s4+(v>>4))|0; s5=(s5+(v>>5))|0; s6=(s6+(v>>6))|0; s7=(s7+(v&31))|0;
          }}
          return [s0,s1,s2,s3,s4,s5,s6,s7].join(",");
        }}
        console.log(scan(dv, 100000));
        buf.transfer();
        var err = "";
        try {{ scan(dv, 8); }} catch (e) {{ err = e.constructor.name; }}
        console.log(err);
        "#
    ));
}

/// W10.3: u32 values ABOVE i32::MAX flowing through spilled homes — the WIDE
/// `+=` accumulators exceed i32 (slot canonicality: the full sign-extended
/// i64 is stored and reloaded; exit boxing from a slot picks the exact
/// double), and the kind-6 getUint32 dst itself may spill (full-qword store
/// after the 32-bit zext write — stale high bits would corrupt the value).
#[test]
fn dvg_parity_spill_u32_wide() {
    assert_matches_node(&format!(
        r#"{PRELUDE}
        function scan(dv, n) {{
          var t0=0,t1=0,t2=0,t3=0,t4=0,t5=0,t6=0;
          for (var i = 0; i < n; i++) {{
            var u = dv.getUint32((i & 1023) << 2);
            t0+=u; t1+=u>>>1; t2+=u>>>2; t3+=u>>>3; t4+=u>>>4; t5+=u>>>5;
            t6+=u>>>6;
          }}
          return [t0,t1,t2,t3,t4,t5,t6].join(",");
        }}
        console.log(scan(dv, 100000));
        "#
    ));
}

/// W10.3: shift counts and compare operands living in spilled homes — the
/// shift arm loads ecx from the slot, the compare arms use qword mem-operand
/// forms (rax-only scratch), and branch flag-fusion must survive the plain
/// slot movs.
#[test]
fn dvg_parity_spill_shift_compare() {
    assert_matches_node(&format!(
        r#"{PRELUDE}
        function scan(dv, n) {{
          var b0=0,b1=0,b2=0,b3=0,b4=0,b5=0;
          var cmp = 0;
          for (var i = 0; i < n; i++) {{
            var le = i & 1; var v = dv.getInt16((i & 2047) << 1, le === 1);
            var sh = i & 7;
            b0=(b0+(v<<sh))|0; b1=(b1+(v>>sh))|0; b2=(b2+(v>>>sh))|0;
            if (b0 > b1) cmp = (cmp + 1)|0;
            if (b2 === b3) cmp = (cmp + 2)|0;
            b3=(b3+(v^b4))|0; b4=(b4+(v&b5))|0; b5=(b5+(v|1))|0;
          }}
          return cmp + ":" + [b0,b1,b2,b3,b4,b5].join(",");
        }}
        console.log(scan(dv, 100000));
        "#
    ));
}

/// W10.3: a spilled home as a DV `pos` operand and Math.imul through spilled
/// operands/dst (imul r32, r/m32 forms; the staged eax dst parks the full
/// canonical qword), with enough live accumulators to overflow the pool.
#[test]
fn dvg_parity_spill_imul_pos() {
    assert_matches_node(&format!(
        r#"{PRELUDE}
        function scan(dv, n) {{
          var m0=0,m1=0,m2=0,m3=0,m4=0,m5=0;
          for (var i = 0; i < n; i++) {{
            var o = (i & 1023) << 2;
            var le = i & 1; var v = dv.getUint32(o, le === 1);
            m0=(m0+Math.imul(v,3))|0; m1=(m1+Math.imul(v,m2))|0;
            m2=(m2+(v&255))|0; m3=(m3+Math.imul(m4,m5))|0;
            m4=(m4+(v>>>16))|0; m5=(m5+(v&65535))|0;
          }}
          return [m0,m1,m2,m3,m4,m5].join(",");
        }}
        console.log(scan(dv, 100000));
        "#
    ));
}

/// Re-run every `dvg_parity_` case in five more modes, each in its own child
/// process (the env latches are read once per process). The same node-derived
/// assertions passing in all six modes is the parity check.
#[test]
fn all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    for (key, val) in [
        ("ZIPP_NO_DV_GPR", "1"),
        ("ZIPP_NO_GPR_HOMES", "1"),
        ("ZIPP_NO_GPR_SPILL_SLOTS", "1"),
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
