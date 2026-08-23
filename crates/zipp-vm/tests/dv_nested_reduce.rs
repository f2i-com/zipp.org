//! Node-parity coverage for the guarded nested-DataView reduction collapse.
//!
//! The native outer region executes one complete inner reduction, measures its
//! exact modulo-2^32 accumulator delta, and may repeat that delta only while a
//! non-shared, attached ArrayBuffer snapshot and the closed static proof hold.

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
        .expect("node v24 on PATH");
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
    assert_eq!(run_ok(src), node_output(src));
}

const LOCAL_PRELUDE: &str = r#"
    "use strict";
    var buf = new ArrayBuffer(64);
    var bytes = new Uint8Array(buf);
    for (var fill = 0; fill < bytes.length; fill++) bytes[fill] = (fill * 151 + 0x83) & 255;
    var dv = new DataView(buf);
    var sum = 0, r = 0, o = 0, last = 0;
    function reduce() {
      for (r = 0; r < 320; r++) {
        for (o = 0; o < 64; o += 4) {
          var le = (o >> 2) & 1;
          var v = dv.getUint32(o, le === 1);
          sum = (sum + (v >>> 24) + (v & 255) +
                 dv.getUint16(o, le === 0) + dv.getInt8(o + 2)) | 0;
          last = v;
        }
      }
    }
"#;

#[test]
fn stable_bytes_and_pre_entry_alias_mutation_match_node() {
    assert_matches_node(&format!(
        r#"{LOCAL_PRELUDE}
        reduce();
        console.log(sum + ":" + r + ":" + o + ":" + last);

        // The compiled region is re-entered with the same DataView identity,
        // but its first real pass must observe the alias's new bytes.
        bytes[0] ^= 255; bytes[3] = 17; bytes[34] ^= 91;
        sum = 0; last = 0;
        reduce();
        console.log(sum + ":" + r + ":" + o + ":" + last);
        "#
    ));
}

#[test]
fn mutation_inside_the_repeated_body_is_not_collapsed() {
    assert_matches_node(
        r#"
        "use strict";
        var buf = new ArrayBuffer(64), bytes = new Uint8Array(buf), dv = new DataView(buf);
        for (var fill = 0; fill < 64; fill++) bytes[fill] = fill * 13 + 7;
        var sum = 0, r = 0, o = 0;
        for (r = 0; r < 320; r++) {
          for (o = 0; o < 64; o += 4) {
            if (r === 151 && o === 0) bytes[0] ^= 255;
            sum = (sum + dv.getUint32(o, (o & 4) === 4)) | 0;
          }
        }
        console.log(sum + ":" + bytes[0] + ":" + r + ":" + o);
        "#,
    );
}

#[test]
fn accumulator_dependent_scratch_state_is_not_collapsed() {
    assert_matches_node(
        r#"
        "use strict";
        var buf = new ArrayBuffer(32), bytes = new Uint8Array(buf), dv = new DataView(buf);
        for (var fill = 0; fill < 32; fill++) bytes[fill] = fill * 17 + 9;
        var sum = 0, scratch = 0, r = 0, o = 0;
        for (r = 0; r < 320; r++) {
          for (o = 0; o < 32; o += 4) {
            // `scratch` is overwritten before it is read in each pass, but its
            // final value changes with the carried accumulator and is visible
            // after the loop. The additive proof must reject this shape.
            scratch = sum;
            sum = (sum + dv.getUint32(o, (o & 4) === 0)) | 0;
          }
        }
        console.log(sum + ":" + scratch + ":" + r + ":" + o);
        "#,
    );
}

#[test]
fn shared_buffer_runtime_guard_keeps_the_ordinary_backedge() {
    assert_matches_node(
        r#"
        "use strict";
        var sab = new SharedArrayBuffer(64), bytes = new Uint8Array(sab), dv = new DataView(sab);
        for (var fill = 0; fill < 64; fill++) bytes[fill] = fill * 29 + 3;
        var sum = 0, r = 0, o = 0;
        for (r = 0; r < 320; r++) {
          for (o = 0; o < 64; o += 4)
            sum = (sum + dv.getInt32(o, (o & 4) === 0)) | 0;
        }
        console.log(sum + ":" + r + ":" + o);
        "#,
    );
}

#[test]
fn compiled_entry_revalidates_detachment() {
    assert_matches_node(&format!(
        r#"{LOCAL_PRELUDE}
        reduce();

        // A detached re-entry must take the normal TypeError path rather than
        // applying a delta captured from the formerly attached buffer.
        buf.transfer();
        sum = 0;
        var err = "";
        try {{ reduce(); }} catch (e) {{ err = e.constructor.name; }}
        console.log("detach:" + err + ":" + r + ":" + o + ":" + sum);
        "#
    ));
}

#[test]
fn wide_accumulator_and_zero_trip_inner_loop_decline_at_runtime() {
    assert_matches_node(
        r#"
        "use strict";
        var buf = new ArrayBuffer(16), dv = new DataView(buf);
        var sum = 9007199254740991, r = 0, o = 0;
        for (r = 0; r < 320; r++) {
          // The body is statically valid but never executes. The native entry
          // therefore continues to see a non-Int32 accumulator.
          for (o = 8; o < 4; o += 4)
            sum = (sum + dv.getUint32(o)) | 0;
        }
        console.log(sum + ":" + r + ":" + o);
        "#,
    );
}

/// The switch is process-latched, so a child runs every semantic case through
/// the ordinary outer backedge using the same test binary.
#[test]
fn zz_off_switch_agrees_with_node() {
    if std::env::var_os("ZIPP_DV_NESTED_REDUCE_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    let out = std::process::Command::new(exe)
        .args(["--skip", "zz_off_switch_agrees_with_node"])
        .env("ZIPP_NO_DV_NESTED_REDUCE", "1")
        .env("ZIPP_DV_NESTED_REDUCE_CHILD", "1")
        .output()
        .expect("re-run test binary");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && !stdout.contains(" 0 passed"),
        "off-switch path diverges:\n--- stdout ---\n{}\n--- stderr ---\n{}",
        stdout,
        String::from_utf8_lossy(&out.stderr)
    );
}
