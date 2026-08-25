//! W7: pinned-guard hoisting on the register tiers (the B119 residual).
//!
//! `hoistable_pins` (plan_region.rs) proves a region cannot invalidate a pin —
//! no in-region write to the pin's source, and every op on the closed
//! no-user-code whitelist — and the emitters then check the pin ONCE at entry
//! (snapshot validity; a miss `entry_bail`s like any failed entry guard)
//! instead of re-loading the source and comparing it against the snapshot at
//! EVERY access. Pinned-STRING `.length` additionally hoists to a prologue
//! fill (immutable receiver ⇒ length is a constant once identity holds).
//!
//! Every `ghoist_parity_*` case asserts byte-identical output against
//! `node -e` at DEFAULT settings (hoisting ON). The final test re-runs the set
//! in five more modes in child processes: `ZIPP_NO_GUARD_HOIST=1` (per-access
//! guards restored — the off-switch must be a pure fallback),
//! `ZIPP_NO_GPR_HOMES=1` (the xmm INT emitter's hoist arms), `ZIPP_JIT_THRESHOLD=1`
//! (compile everything immediately), `ZIPP_GC_STRESS=1` (string receivers
//! across GC — heap indices don't move, snapshots re-derive per entry), and
//! `ZIPP_NOJIT=1` (pure interpreter).

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
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

/// The fnv1a shape (a PARAM receiver ⇒ `TaPinSrc::Reg` pin): identity hoisted
/// to entry, `str.length` hoisted to a prologue fill, charCodeAt keeps only
/// its bounds check. Also the empty and one-char edge receivers.
#[test]
fn ghoist_parity_fnv1a_charcode() {
    assert_matches_node(
        r#"
        "use strict";
        function fnv1a(str) {
          var h = 0x811c9dc5;
          for (var i = 0; i < str.length; i++) {
            h = Math.imul(h ^ str.charCodeAt(i), 16777619);
          }
          return h >>> 0;
        }
        var s = "";
        for (var i = 0; i < 400; i++) s += String.fromCharCode(32 + (i * 7) % 90);
        var acc = 0;
        for (var r = 0; r < 50; r++) acc = (acc + fnv1a(s)) | 0;
        console.log(acc, fnv1a(""), fnv1a("a"));
        "#,
    );
}

/// A GLOBAL string receiver reassigned BETWEEN OSR entries: valid ASCII →
/// non-ASCII (the snapshot declines to {0,0,0}, so the hoisted ENTRY guard
/// must bail — the per-access identity compare it replaced is gone) → a
/// number (`.length` undefined ⇒ the loop never runs) → ASCII again
/// (re-snapshot revalidates by construction).
#[test]
fn ghoist_parity_receiver_swapped_between_entries() {
    assert_matches_node(
        r#"
        "use strict";
        var s = "the quick brown fox jumps over the lazy dog 0123456789";
        function hash() {
          var h = 5381;
          for (var i = 0; i < s.length; i++) {
            h = Math.imul(h, 33) ^ s.charCodeAt(i);
          }
          return h >>> 0;
        }
        var a = 0;
        for (var r = 0; r < 300; r++) a = (a + hash()) | 0;
        s = "héllo wörld ☃ surrogate 😀 pair";
        var b = 0;
        for (var r = 0; r < 300; r++) b = (b + hash()) | 0;
        s = 12345;
        var c = 0;
        for (var r = 0; r < 300; r++) c = (c + hash()) | 0;
        s = "plain ascii once more";
        var d = 0;
        for (var r = 0; r < 300; r++) d = (d + hash()) | 0;
        console.log(a, b, c, d);
        "#,
    );
}

/// The receiver written INSIDE the loop. Two variants: a `Move`-defined
/// alternating receiver (the pin is never built — the per-op helper path must
/// stay node-identical), and a receiver redefined by `Math.imul` — the ONE
/// admitted op the pin builder's write cover misses, so a pin EXISTS while
/// the region writes its source register. The hoist predicate must refuse it
/// (it uses the emitter-grade `writes_reg`, which sees MathOp dsts), and the
/// surviving per-access path must still answer like node (a TypeError once
/// the receiver is a number).
#[test]
fn ghoist_parity_receiver_written_in_region() {
    assert_matches_node(
        r#"
        "use strict";
        function alt(a, b, n) {
          var t = a, s = 0;
          for (var i = 0; i < n; i++) {
            s = (s + t.charCodeAt(i % t.length)) | 0;
            t = (i & 1) ? b : a;
          }
          return s;
        }
        console.log(alt("abcdefgh", "ZYXWVUTS", 4000));
        function imulRecv(a, n) {
          var t = a, s = 0;
          try {
            for (var i = 0; i < n; i++) {
              s = (s + (t.charCodeAt(i & 3) | 0)) | 0;
              if (i === n - 5) t = Math.imul(i, 0);
            }
            return "no-throw:" + s;
          } catch (e) {
            return e.name + ":" + s;
          }
        }
        console.log(imulRecv("abcdefgh", 4000));
        "#,
    );
}

/// Detach/resize INSIDE the loop is impossible in an eligible region by
/// construction — `transfer()`/`resize()` are CallMethods outside the
/// whitelist, so such regions never reach the register tiers, let alone
/// hoist. The unhoisted (memory-tier / helper) path must stay node-identical
/// across the detach and the shrink.
#[test]
fn ghoist_parity_ta_detach_resize_in_loop() {
    assert_matches_node(
        r#"
        "use strict";
        var b = new ArrayBuffer(32);
        var t = new Int32Array(b);
        for (var i = 0; i < 8; i++) t[i] = (i * 37 + 5) | 0;
        function sumWithTransfer(n, k) {
          var s = 0;
          for (var i = 0; i < n; i++) {
            s = (s + (t[i & 7] | 0)) | 0;
            if (i === k) b.transfer();
          }
          return s;
        }
        console.log(sumWithTransfer(4000, 2000), t.length);
        var rb = new ArrayBuffer(32, {maxByteLength: 64});
        var rt = new Int32Array(rb);
        for (var i = 0; i < 8; i++) rt[i] = (i * 91 + 3) | 0;
        function sumWithResize(n, k) {
          var s = 0;
          for (var i = 0; i < n; i++) {
            s = (s + (rt[i & 7] | 0)) | 0;
            if (i === k) rb.resize(16);
          }
          return s;
        }
        console.log(sumWithResize(4000, 2000), rt.length);
        "#,
    );
}

/// Deopt mid-loop, then re-entry: an OOB charCodeAt (the interpreter yields
/// NaN — unrepresentable in an i64 home, so the access deopts AT its ip) and
/// an accumulator crossing non-int and back. Every re-entry re-runs the
/// prologue, so the hoisted entry guard revalidates by construction.
#[test]
fn ghoist_parity_deopt_midloop_reentry() {
    assert_matches_node(
        r#"
        "use strict";
        function scan(s, n) {
          var acc = 0;
          for (var i = 0; i < n; i++) {
            var k = (i === 900 || i === 2100) ? 500 : (i & 7);
            acc = (acc + (s.charCodeAt(k) | 0)) | 0;
          }
          return acc;
        }
        console.log(scan("abcdefgh", 4000));
        function mix(s, n) {
          var h = 1;
          for (var i = 0; i < n; i++) {
            h = Math.imul(h ^ s.charCodeAt(i % s.length), 31);
            if (i === 700) h = h + 0.5;
            if (i === 800) h = (h - 0.5) | 0;
          }
          return h;
        }
        console.log(mix("hoisted guards", 3000));
        "#,
    );
}

/// The typedarray-math DV swizzle shape on the DOUBLE tier (split receivers +
/// fused endian Eq + hoisted identity), plus an OOB pos mid-loop: the bounds
/// guard STAYS per-access, deopts, and the interpreter raises node's
/// RangeError.
#[test]
fn ghoist_parity_dv_swizzle() {
    assert_matches_node(
        r#"
        "use strict";
        var ib = new Int32Array(64);
        for (var i = 0; i < 64; i++) ib[i] = (Math.imul(i + 3, 2654435761) ^ (i << 7)) | 0;
        var dv = new DataView(ib.buffer, 0, 256);
        function swiz(n) {
          var bsum = 0;
          for (var r = 0; r < n; r++) {
            for (var o = 0; o < 256; o += 4) {
              var le = (o >> 2) & 1;
              var v = dv.getUint32(o, le === 1);
              bsum = (bsum + (v >>> 24) + (v & 255) + dv.getUint16(o, le === 0) + dv.getInt8(o + 2)) | 0;
            }
          }
          return bsum;
        }
        console.log(swiz(300));
        function swizOob(n, bad) {
          var bsum = 0;
          try {
            for (var o = 0; o < n; o += 4) {
              var le = (o >> 2) & 1;
              var p = (o === bad) ? 255 : (o & 252);
              bsum = (bsum + dv.getUint16(p, le === 1)) | 0;
            }
          } catch (e) {
            return bsum + ":" + e.name;
          }
          return "" + bsum;
        }
        console.log(swizOob(20000, 16000));
        "#,
    );
}

/// Dense all-Int Array: `for (i < a.length) s += a[i]` (identity hoisted for
/// both the length read and the element load on the INT tier), the array
/// REPLACED between OSR entries, and a double element appearing mid-array
/// (the per-access tag guard stays and deopts).
#[test]
fn ghoist_parity_dense_array_length_and_elements() {
    assert_matches_node(
        r#"
        "use strict";
        var arr = [];
        for (var i = 0; i < 512; i++) arr.push((i * 13 - 700) | 0);
        function asum() {
          var s = 0;
          for (var i = 0; i < arr.length; i++) s = (s + arr[i]) | 0;
          return s;
        }
        var t1 = 0;
        for (var r = 0; r < 300; r++) t1 = (t1 + asum()) | 0;
        arr = arr.slice(0, 256);
        var t2 = 0;
        for (var r = 0; r < 300; r++) t2 = (t2 + asum()) | 0;
        arr[128] = 0.5;
        var t3 = 0;
        for (var r = 0; r < 300; r++) t3 = (t3 + asum()) | 0;
        console.log(t1, t2, t3);
        "#,
    );
}

/// Pinned Int32Array read/write kernels (identity hoisted; the store's bounds
/// guard stays — an OOB store must still deopt to the interpreter's silent
/// no-op), with a key that leaves range on a schedule.
#[test]
fn ghoist_parity_int32array_kernel_oob_store() {
    assert_matches_node(
        r#"
        "use strict";
        var a = new Int32Array(8);
        function kernel(n) {
          var s = 0;
          for (var i = 0; i < n; i++) {
            var k = (i & 63) === 63 ? 99 : (i & 7);
            a[k] = (a[k] + i) | 0;
            s = (s + a[i & 7]) | 0;
          }
          return s;
        }
        console.log(kernel(4000), a.join(","));
        "#,
    );
}

/// Everything above must answer identically in every mode. The parity tests
/// re-run in child processes; their node-derived assertions passing in all
/// modes IS the parity check.
#[test]
fn all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    let modes: [&[(&str, &str)]; 5] = [
        &[("ZIPP_NO_GUARD_HOIST", "1")],
        &[("ZIPP_NO_GPR_HOMES", "1")],
        &[("ZIPP_JIT_THRESHOLD", "1")],
        &[("ZIPP_GC_STRESS", "1")],
        &[("ZIPP_NOJIT", "1")],
    ];
    for mode in modes {
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("ghoist_parity_");
        for (key, val) in mode {
            cmd.env(key, val);
        }
        let out = cmd.output().expect("spawn the test binary");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{mode:?} mode failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !stdout.contains("running 0 tests"),
            "the ghoist_parity_ filter matched nothing under {mode:?}:\n{stdout}"
        );
    }
}
