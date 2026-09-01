//! Fixed-width integer TypedArray reads on the unboxed INTEGER tiers.
//!
//! The widening is read-only and deliberately excludes Uint32, floats and
//! BigInt arrays.  Every native access still uses the existing live receiver,
//! kind, buffer, effective-length and bounds snapshot guards; a miss resumes at
//! the GetIndex bytecode so the interpreter supplies the complete exotic-key
//! semantics.

fn run_ok(src: &str) -> Vec<String> {
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
        .expect("node on PATH");
    assert!(
        out.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .expect("node output is UTF-8")
        .lines()
        .map(str::to_owned)
        .collect()
}

fn assert_matches_node(src: &str) {
    assert_eq!(run_ok(src), node_output(src), "zipp != node for:\n{src}");
}

fn kernel_source(ctor: &str, values: &str) -> String {
    format!(
        r#"
        "use strict";
        function kernel(a, n) {{
          let s = 0;
          for (let i = 0; i < n; i++) {{
            let k = i & 7;
            // These three rare keys must leave at the precise GetIndex and let
            // the interpreter apply IntegerIndexedElementGet semantics.
            if (i === 701) k = -1;
            else if (i === 1301) k = 1.5;
            else if (i === 1901) k = 99;
            let v = a[k];
            s = (Math.imul(s ^ (v | 0), 33) + i) | 0;
          }}
          return s;
        }}
        let a = new {ctor}([{values}]);
        console.log(kernel(a, 64), kernel(a, 2400));
        "#
    )
}

/// Signedness, zero-extension and the Uint8Clamped read representation all
/// match Node at boundary values. Uint32/float/BigInt cases are included as
/// differential decline controls: they must stay correct on their lower tier.
#[test]
fn narrow_ta_parity_kind_matrix() {
    for (ctor, values) in [
        ("Int8Array", "-128,-127,-1,0,1,2,126,127"),
        ("Uint8Array", "0,1,2,127,128,254,255,256"),
        ("Uint8ClampedArray", "-1,0,1,127,128,254,255,300"),
        ("Int16Array", "-32768,-32767,-1,0,1,2,32766,32767"),
        ("Uint16Array", "0,1,2,32767,32768,65534,65535,65536"),
        (
            "Int32Array",
            "-2147483648,-2147483647,-1,0,1,2,2147483646,2147483647",
        ),
        (
            "Uint32Array",
            "0,1,2147483647,2147483648,4294967294,4294967295,7,9",
        ),
        ("Float32Array", "-1.5,-0,0,1.25,2.5,3.75,NaN,Infinity"),
        ("Float64Array", "-1.5,-0,0,1.25,2.5,3.75,NaN,Infinity"),
    ] {
        assert_matches_node(&kernel_source(ctor, values));
    }
    // BigInt arrays are an explicit lower-tier control. Their element result
    // allocates and cannot inhabit the integer lane used above.
    assert_matches_node(
        r#"
        function big(a,n){let s=0n;for(let i=0;i<n;i++)s=(s^a[i&7])+BigInt(i&255);return s}
        let a=new BigInt64Array([-9223372036854775808n,-1n,0n,1n,2n,3n,7n,9223372036854775807n]);
        let b=new BigUint64Array([0n,1n,2n,3n,7n,9n,18446744073709551614n,18446744073709551615n]);
        console.log(String(big(a,2400)),String(big(b,2400)));
        "#,
    );
}

/// A compiled pin is only a snapshot. Mutation is visible on the next read;
/// detach, fixed-view OOB after shrink, length-tracking resize and restoration
/// all invalidate/refill the snapshot rather than leaving a stale raw pointer.
#[test]
fn narrow_ta_parity_mutation_detach_resize() {
    assert_matches_node(
        r#"
        "use strict";
        function scan(a, n) {
          let s = 0;
          for (let i = 0; i < n; i++) s = (Math.imul(s ^ (a[i & 7] | 0), 33) + i) | 0;
          return s;
        }

        let m = new Uint8Array([0,1,127,128,254,255,3,9]);
        let m0 = scan(m, 1200); m[3] = 17; let m1 = scan(m, 1200);

        let db = new ArrayBuffer(8), d = new Uint8Array(db);
        d.set(m); let d0 = scan(d, 1200); db.transfer(); let d1 = scan(d, 200);

        let rb = new ArrayBuffer(8, {maxByteLength: 32});
        let fixed = new Uint8Array(rb, 0, 8), tracking = new Uint8Array(rb);
        fixed.set([1,2,3,4,5,6,7,8]);
        let r0 = scan(fixed, 1200) + "/" + scan(tracking, 1200);
        rb.resize(4);
        let r1 = scan(fixed, 200) + "/" + scan(tracking, 1200);
        rb.resize(16); tracking.set([9,10,11,12,13,14,15,16], 4);
        let r2 = scan(fixed, 1200) + "/" + scan(tracking, 1200);

        console.log(m0, m1, d0, d1, r0, r1, r2, fixed.length, tracking.length);
        "#,
    );
}

/// Child target for the JITLOG mechanism assertion below.
#[test]
fn narrow_ta_parity_mechanism_probe() {
    assert_matches_node(
        r#"
        function kernel(a,n){let s=0;for(let i=0;i<n;i++)s=(Math.imul(s^(a[i&7]|0),33)+i)|0;return s}
        let a=new Uint8Array([0,1,2,3,4,5,254,255]);
        console.log(kernel(a,64),kernel(a,2400));
        "#,
    );
}

/// Default mode must actually compile a narrow TA read on the INTEGER tier;
/// the kill switch must restore the old decline. Uint32 remains a decline even
/// with the widening enabled.
#[test]
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn narrow_ta_mechanism_and_decline_controls() {
    let exe = std::env::current_exe().expect("test exe path");
    let run = |extra: &[(&str, &str)]| {
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("narrow_ta_parity_mechanism_probe")
            .arg("--nocapture")
            .env("ZIPP_JITLOG", "1")
            .env("ZIPP_JITDECLINE", "1");
        for &(k, v) in extra {
            cmd.env(k, v);
        }
        cmd.output().expect("spawn test binary")
    };
    let on = run(&[]);
    assert!(
        on.status.success(),
        "default child failed: {}",
        String::from_utf8_lossy(&on.stderr)
    );
    let on_log = String::from_utf8_lossy(&on.stderr);
    assert!(
        on_log.contains("INT region") || on_log.contains("INT-GPR region"),
        "Uint8 probe did not engage INTEGER tier:\n{on_log}"
    );

    let off = run(&[("ZIPP_NO_INT_TA_NARROW_READS", "1")]);
    assert!(
        off.status.success(),
        "off child failed: {}",
        String::from_utf8_lossy(&off.stderr)
    );
    let off_log = String::from_utf8_lossy(&off.stderr);
    assert!(
        off_log.contains("GetIndex") && off_log.contains("int-reject"),
        "off switch did not restore GetIndex decline:\n{off_log}"
    );
}

/// The differential corpus is replayed with the off switch, eager JIT, GC at
/// every opportunity, and the interpreter. Each mode gets a fresh process so
/// process-latched JIT controls cannot contaminate one another.
#[test]
fn narrow_ta_all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    for mode in [
        ("ZIPP_NO_INT_TA_NARROW_READS", "1"),
        ("ZIPP_JIT_THRESHOLD", "1"),
        ("ZIPP_GC_STRESS", "1"),
        ("ZIPP_NOJIT", "1"),
    ] {
        let out = std::process::Command::new(&exe)
            .arg("narrow_ta_parity_")
            .env(mode.0, mode.1)
            .output()
            .expect("spawn test binary");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{mode:?} failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !stdout.contains("running 0 tests"),
            "mode filter matched nothing: {stdout}"
        );
    }
}

/// Sandboxed/metered execution charges the same bytecode count as forced
/// interpreter execution and returns the same value. Native reads introduce no
/// unmetered helper work or extra replay of a charged instruction.
#[test]
#[cfg(feature = "instrument")]
fn narrow_ta_meter_is_exact() {
    const SCRIPT: &str = r#"
      function k(a,n){let s=0;for(let i=0;i<n;i++)s=(Math.imul(s^(a[i&7]|0),33)+i)|0;return s}
      k(new Uint8Array([0,1,2,3,127,128,254,255]), 20000)
    "#;
    fn metered(interpreter_only: bool) -> (u64, String) {
        let mut state =
            zipp_vm::embed::compile_script("var ready=true;").expect("bootstrap compiles");
        state.set_limits(20_000_000, None);
        if interpreter_only {
            state.disable_vm_jit();
        }
        state.run_init().expect("bootstrap runs");
        let before = state.steps_remaining();
        let expr = format!("globalThis.__narrowTaResult=(0,eval)({SCRIPT:?});");
        state.eval_in_context(&expr).expect("kernel runs");
        let used = before - state.steps_remaining();
        let value = state
            .eval_in_context("String(globalThis.__narrowTaResult)")
            .expect("result reads")
            .as_str()
            .expect("string result")
            .to_owned();
        (used, value)
    }
    assert_eq!(metered(false), metered(true));
}
