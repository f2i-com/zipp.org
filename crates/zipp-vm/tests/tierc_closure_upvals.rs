//! Tier-C closure/upvalue coverage for the hostile rotating-closure lane.
//!
//! The native whole-function tier has no interpreter `Frame` while reached by
//! a cross-call, so captured accesses must use the exact live closure selected
//! at that call site. These tests rotate more identities than the call IC has
//! ways, vary lexical `this`, and force accessor re-entry / a throw after a
//! captured write. The process sweep compares the optimized route with each
//! bounded off-switch and the interpreter oracle; processes are required
//! because JIT switches are intentionally latched.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

const ROTATING_PROBE: &str = r#"
    "use strict";
    (function main() {
      const rounds = 120000;

      function makePipeline(seed) {
        let calls = 0;
        let offset = seed | 0;

        function rotate(value, amount = 5) {
          return (value << amount) | (value >>> (32 - amount));
        }

        return (value, salt) => {
          calls = (calls + 1) | 0;
          if ((calls & 1023) === 0) {
            offset = (offset + seed + calls) | 0;
          }
          value = Math.imul(value ^ salt ^ offset, 1664525);
          return (rotate(value, (seed & 7) + 1) + 1013904223) | 0;
        };
      }

      const pipelines = [];
      for (let i = 0; i < 16; i++) {
        pipelines.push(makePipeline((i * 97 + 11) | 0));
      }

      let state = 0x13579bdf | 0;
      let checksum = 0;
      for (let i = 0; i < rounds; i++) {
        const fn = pipelines[i & 15];
        state = fn(state, i & 1023);
        checksum = (checksum + (state & 65535)) | 0;
      }
      console.log("probe", state, checksum, rounds, pipelines.length);
    })();
"#;

const REBOUND_FID_PROBE: &str = r#"
    "use strict";
    (function () {
      function make(seed) {
        let state = seed | 0;
        return (value, amount) => {
          state = (state + amount) | 0;
          let pad = (value ^ state) | 0;
          pad = Math.imul(pad, 3);
          pad = (pad + 11) | 0;
          pad = (pad ^ 85) | 0;
          pad = (pad - 9) | 0;
          pad = Math.imul(pad, 5);
          pad = pad >>> 1;
          pad = (pad << 1) | 0;
          const zero = (pad ^ pad) | 0;
          return (state + zero) | 0;
        };
      }

      const funcs = [];
      for (let i = 0; i < 16; i++) funcs.push(make((i * 17 + 3) | 0));
      let replacementCalls = 0;
      function replacement(value, amount) {
        replacementCalls++;
        return (Math.imul(value, 3) + amount + 17) | 0;
      }

      let checksum = 0;
      for (let i = 0; i < 120000; i++) {
        // The loop region and its same-prototype descriptor are hot before this
        // mutation. A different live fid must miss the pure specialized prefix,
        // then execute exactly once through the unchanged call-IC fallback.
        if (i === 60000) funcs[3] = replacement;
        const fn = funcs[i & 15];
        checksum = (checksum + fn(i & 255, (i & 7) + 1)) | 0;
      }
      console.log("rebound:" + checksum + ":" + replacementCalls);
    })();
"#;

const XORSHIFT_PROBE: &str = r#"
    "use strict";
    (function () {
      function make(seed) {
        let state = seed;
        function step() {
          state ^= state << 13;
          state ^= state >>> 17;
          state ^= state << 5;
          // Keep this over Tier C's deliberately conservative medium-body
          // threshold while preserving the generic xorshift return value.
          return state | 0;
        }
        return [step, (next) => { state = next; }];
      }

      const pairs = [];
      for (let i = 0; i < 16; i++) pairs.push(make((i * 97 + 11) | 0));
      let checksum = 0;
      for (let i = 0; i < 80000; i++) {
        checksum = (checksum + pairs[i & 15][0]()) | 0;
      }

      const pair = pairs[0];
      pair[1](2147483647);
      const max = pair[0]();
      pair[1](-1);
      const negative = pair[0]();
      pair[1](3.5);
      const fractional = pair[0]();
      let coercions = 0;
      pair[1]({ valueOf: function () { coercions++; return 0x12345678; } });
      const object = pair[0]();
      const afterObject = pair[0]();
      pair[1]("40");
      const string = pair[0]();
      console.log(
        "xorshift:" + checksum + ":" + max + ":" + negative + ":" +
        fractional + ":" + coercions + ":" + object + ":" +
        afterObject + ":" + string
      );
    })();
"#;

const GLOBAL_XORSHIFT_PROBE: &str = r#"
    "use strict";
    let globalXorState = 11;
    function globalXorStep() {
      globalXorState ^= globalXorState << 13;
      globalXorState ^= globalXorState >>> 17;
      globalXorState ^= globalXorState << 5;
      return globalXorState | 0;
    }
    let globalXorChecksum = 0;
    for (let i = 0; i < 80000; i++) {
      globalXorChecksum = (globalXorChecksum + globalXorStep()) | 0;
    }
    globalXorState = 2147483647;
    const globalMax = globalXorStep();
    globalXorState = -1;
    const globalNegative = globalXorStep();
    globalXorState = 3.5;
    const globalFractional = globalXorStep();
    let globalCoercions = 0;
    globalXorState = {
      valueOf: function () { globalCoercions++; return 0x12345678; }
    };
    const globalObject = globalXorStep();
    const globalAfterObject = globalXorStep();
    globalXorState = "40";
    const globalString = globalXorStep();
    console.log(
      "global-xorshift:" + globalXorChecksum + ":" + globalMax + ":" +
      globalNegative + ":" + globalFractional + ":" + globalCoercions +
      ":" + globalObject + ":" + globalAfterObject + ":" + globalString
    );
"#;

#[test]
fn tierc_closure_semantics_rotating_mutable_captures() {
    assert_eq!(
        run_ok(ROTATING_PROBE),
        ["probe 1982437222 -360076033 120000 16"]
    );
}

#[test]
fn tierc_closure_semantics_different_fid_rebind_falls_back_once() {
    assert_eq!(run_ok(REBOUND_FID_PROBE), ["rebound:1957807788:3750"]);
}

#[test]
fn tierc_closure_semantics_rotating_arrows_keep_lexical_this() {
    let out = run_ok(
        r#"
        "use strict";
        (function () {
          function Owner(tag) {
            this.tag = tag;
            this.make = function () {
              let calls = 0;
              return (input, ignored) => {
                calls = (calls + 1) | 0;
                let pad = (input + 3) | 0;
                pad = (pad * 3) | 0;
                pad = (pad + 11) | 0;
                pad = (pad ^ 85) | 0;
                pad = (pad - 9) | 0;
                pad = (pad * 5) | 0;
                pad = pad >>> 1;
                pad = (pad << 1) | 0;
                pad = (pad + 7) | 0;
                pad = (pad ^ 123) | 0;
                pad = (pad - 123) | 0;
                const zero = ((pad ^ pad) + (ignored ^ ignored)) | 0;
                return (this.tag * 1000000 + calls + zero) | 0;
              };
            };
          }

          const funcs = [];
          for (let i = 0; i < 16; i++) {
            funcs.push(new Owner(i + 1).make());
          }
          let exact = true;
          for (let i = 0; i < 32000; i++) {
            const which = i & 15;
            const expected = (which + 1) * 1000000 + (i >>> 4) + 1;
            if (funcs[which](i & 7, which) !== expected) exact = false;
          }
          console.log(exact + ":" + funcs[0](0, 0) + ":" + funcs[15](0, 15));
        })();
        "#,
    );
    assert_eq!(out, ["true:1002001:16002001"]);
}

#[test]
fn tierc_closure_semantics_reentry_and_throw_resume_exact_ip() {
    let out = run_ok(
        r#"
        "use strict";
        (function () {
          const runs = [];
          const sets = [];
          const reads = [];

          function make(id) {
            let count = 0;
            let tail = 0;
            let target = { x: id + 1 };
            const run = (input) => {
              count = (count + 1) | 0;
              let pad = (input + 3) | 0;
              pad = (pad * 3) | 0;
              pad = (pad + 11) | 0;
              pad = (pad ^ 85) | 0;
              pad = (pad - 9) | 0;
              pad = (pad * 5) | 0;
              pad = pad >>> 1;
              pad = (pad << 1) | 0;
              pad = (pad + 7) | 0;
              pad = (pad ^ 123) | 0;
              pad = (pad - 123) | 0;
              const got = target.x;
              tail = (tail + got + ((pad ^ pad) | 0)) | 0;
              return tail;
            };
            return [run, (next) => { target = next; }, () => count + "/" + tail];
          }

          for (let i = 0; i < 16; i++) {
            const pack = make(i);
            runs.push(pack[0]);
            sets.push(pack[1]);
            reads.push(pack[2]);
          }

          const nested = runs[2];
          let reentries = 0;
          const tricky = {};
          Object.defineProperty(tricky, "x", {
            get: function () {
              // Allocation is deliberate: under ZIPP_GC_STRESS this collects
              // while the outer frame-free Tier-C activation is suspended.
              const fresh = { value: 7 };
              reentries++;
              nested(0);
              return fresh.value;
            }
          });

          let thrown = 0;
          let wrongThrow = false;
          for (let i = 0; i < 24000; i++) {
            if (i === 20000) sets[0](tricky);
            if (i === 22000) sets[1](null);
            const fn = runs[i & 15];
            try {
              fn(i & 7);
            } catch (e) {
              if (e instanceof TypeError) thrown++;
              else wrongThrow = true;
            }
          }

          console.log(
            thrown + ":" + reentries + ":" + wrongThrow + ":" +
            reads[0]() + ":" + reads[1]() + ":" + reads[2]()
          );
        })();
        "#,
    );
    // The null receiver throws AFTER count's UpvalSet. Resuming the bailout at
    // the call or function entry would apply that write twice (1625, not 1500).
    // The accessor calls closure 2 while closure 0 is suspended; the final
    // UpvalSet must see closure 0 again after that re-entry.
    assert_eq!(out, ["125:250:false:1500/3000:1500/2750:1750/5250"]);
}

#[test]
fn tierc_closure_semantics_forwarded_heap_values_survive_gc() {
    let out = run_ok(
        r#"
        "use strict";
        (function () {
          const puts = [];
          const reads = [];
          function make(initial) {
            let cell = initial;
            function put(next, salt) {
              cell = next;
              const seen = cell;
              let pad = (salt + 3) | 0;
              pad = (pad * 3) | 0;
              pad = (pad + 11) | 0;
              pad = (pad ^ 85) | 0;
              pad = (pad - 9) | 0;
              pad = (pad * 5) | 0;
              pad = pad >>> 1;
              pad = (pad << 1) | 0;
              pad = (pad + 7) | 0;
              pad = (pad ^ 123) | 0;
              pad = (pad - 123) | 0;
              if ((pad ^ pad) !== 0) return null;
              return seen;
            }
            return [put, () => cell];
          }

          for (let i = 0; i < 16; i++) {
            const pair = make(i);
            puts.push(pair[0]);
            reads.push(pair[1]);
          }
          const expected = [];
          let exact = true;
          for (let i = 0; i < 16000; i++) {
            const which = i & 15;
            let next;
            if ((i & 3) === 0) next = { value: i };
            else if ((i & 3) === 1) next = "v" + i;
            else if ((i & 3) === 2) next = i + 0.5;
            else next = null;
            expected[which] = next;
            if (puts[which](next, i) !== next) exact = false;
          }
          for (let i = 0; i < 16; i++) {
            if (reads[i]() !== expected[i]) exact = false;
          }
          console.log("forward-gc:" + exact);
        })();
        "#,
    );
    assert_eq!(out, ["forward-gc:true"]);
}

#[test]
fn tierc_closure_semantics_jump_target_never_uses_textual_predecessor() {
    let out = run_ok(
        r#"
        "use strict";
        function makeLoop() {
          let value = 0;
          return function (limit) {
            value = 1;
            for (;;) {
              const seen = value;
              let pad = (seen + 3) | 0;
              pad = (pad * 3) | 0;
              pad = (pad + 11) | 0;
              pad = (pad ^ 85) | 0;
              pad = (pad - 9) | 0;
              pad = (pad * 5) | 0;
              pad = pad >>> 1;
              pad = (pad << 1) | 0;
              if (pad === -999999) return pad;
              if (seen >= limit) return seen;
              value = (seen + 1) | 0;
            }
          };
        }
        const loop = makeLoop();
        let sum = 0;
        for (let i = 0; i < 16000; i++) sum += loop((i & 7) + 1);
        console.log("target:" + sum);
        "#,
    );
    assert_eq!(out, ["target:72000"]);
}

#[test]
fn tierc_closure_semantics_fused_i32_increment_wraps_and_deopts_purely() {
    let out = run_ok(
        r#"
        "use strict";
        function makeCounter() {
          let count = 0;
          function bump(salt) {
            count = (count + 1) | 0;
            let pad = (salt + 3) | 0;
            pad = (pad * 3) | 0;
            pad = (pad + 11) | 0;
            pad = (pad ^ 85) | 0;
            pad = (pad - 9) | 0;
            pad = (pad * 5) | 0;
            pad = pad >>> 1;
            pad = (pad << 1) | 0;
            pad = (pad + 7) | 0;
            pad = (pad ^ 123) | 0;
            if (pad === -999999) return pad;
            return count;
          }
          return [bump, (next) => { count = next; }];
        }

        const pair = makeCounter();
        const bump = pair[0];
        const set = pair[1];
        for (let i = 0; i < 1000; i++) bump(i);

        set(2147483646);
        const wraps = [bump(1), bump(2), bump(3)];
        let coercions = 0;
        set({ valueOf: function () { coercions++; return 40; } });
        const objectResult = bump(4);
        const afterObject = bump(5);
        set(3.5);
        const doubleResult = bump(6);
        set("40");
        const stringResult = bump(7);
        console.log(
          wraps.join(",") + "|" + coercions + ":" + objectResult + ":" +
          afterObject + ":" + doubleResult + ":" + stringResult
        );
        "#,
    );
    assert_eq!(out, ["2147483647,-2147483648,-2147483647|1:41:42:4:401"]);
}

#[test]
fn tierc_closure_semantics_fused_i32_xorshift_chain_is_exact_and_pure() {
    assert_eq!(
        run_ok(XORSHIFT_PROBE),
        ["xorshift:1535512503:-2146721761:253983:811107:2:-2020058459:358294691:10814826"]
    );
}

#[test]
fn tierc_global_semantics_fused_i32_xorshift_chain_is_exact_and_pure() {
    assert_eq!(
        run_ok(GLOBAL_XORSHIFT_PROBE),
        ["global-xorshift:1778999344:-2146721761:253983:811107:2:-2020058459:358294691:10814826"]
    );
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[test]
fn tierc_closure_mechanism_child() {
    if std::env::var_os("ZIPP_TIERC_CLOSURE_CHILD").is_none() {
        return;
    }
    assert_eq!(
        run_ok(ROTATING_PROBE),
        ["probe 1982437222 -360076033 120000 16"]
    );
    assert_eq!(
        run_ok(XORSHIFT_PROBE),
        ["xorshift:1535512503:-2146721761:253983:811107:2:-2020058459:358294691:10814826"]
    );
    assert_eq!(
        run_ok(GLOBAL_XORSHIFT_PROBE),
        ["global-xorshift:1778999344:-2146721761:253983:811107:2:-2020058459:358294691:10814826"]
    );
    let (fast, full) = zipp_vm::cross_fill_stats();
    // The default route now inlines the nested capture-free rotate leaf. Keep
    // the original cross-call mechanism assertion under the leaf off-switch so
    // both independent routes remain covered instead of one masking the other.
    if std::env::var_os("ZIPP_NO_POLY_LEAF_INLINE").is_some() {
        assert!(
            fast + full > 100000,
            "rotating same-proto call site never reached native cross-call: fast={fast}, full={full}"
        );
    } else {
        // The remaining outer rotating-closure call has UpvalGet/UpvalSet in
        // its Tier-C body. cross_uninit_mask must model those ops so its
        // already-initialized register window takes the selective fast fill;
        // u64::MAX would force essentially every execution through full fill.
        assert!(
            fast > 100000 && fast > full.saturating_mul(100),
            "captured Tier-C cross-call did not use selective window fill: fast={fast}, full={full}"
        );
    }
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[test]
fn tierc_closure_rotating_mechanism_engages() {
    let exe = std::env::current_exe().expect("test binary path");
    let out = std::process::Command::new(&exe)
        .args(["tierc_closure_mechanism_child", "--exact", "--nocapture"])
        .env("ZIPP_TIERC_CLOSURE_CHILD", "1")
        .env("ZIPP_ICSTATS", "1")
        .env("ZIPP_JITLOG", "1")
        // Eight rotations fill the live IC with multiple identities before
        // OSR selection; threshold one is intentionally still monomorphic.
        .env("ZIPP_JIT_THRESHOLD", "8")
        .env_remove("ZIPP_NOJIT")
        .env_remove("ZIPP_NO_TIERC_UPVAL")
        .env_remove("ZIPP_NO_POLY_CROSSCALL")
        .env_remove("ZIPP_NO_SAME_PROTO_CROSS2")
        .env_remove("ZIPP_NO_CROSSCALL")
        .env_remove("ZIPP_NO_TIERC_UPVAL_FORWARD")
        .env_remove("ZIPP_NO_TIERC_UPVAL_INC_I32")
        .env_remove("ZIPP_NO_TIERC_UPVAL_XORSHIFT")
        .env_remove("ZIPP_NO_TIERC_GLOBAL_XORSHIFT")
        .env_remove("ZIPP_NO_POLY_LEAF_INLINE")
        .output()
        .expect("spawn mechanism child");
    assert!(
        out.status.success(),
        "mechanism child failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("SAME-PROTO-ARROW2 fn2 callee_regs=23"),
        "mechanism child did not select the exact same-prototype cross-call lane:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("Tier-C upval-forward sites=1"),
        "mechanism child did not compile the captured-value forwarding site:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("Tier-C upval-inc-i32 sites=1"),
        "mechanism child did not compile the captured i32 increment fusion:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("Tier-C upval-xorshift chains=1 steps=3"),
        "mechanism child did not compile the captured i32 xorshift chain:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("Tier-C global-xorshift chains=1 steps=3"),
        "mechanism child did not compile the global i32 xorshift chain:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("SAME-PROTO-INLINE (default_mask=0x2)"),
        "mechanism child did not inline the rotating default-parameter leaf:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let cross = std::process::Command::new(exe)
        .args(["tierc_closure_mechanism_child", "--exact", "--nocapture"])
        .env("ZIPP_TIERC_CLOSURE_CHILD", "1")
        .env("ZIPP_ICSTATS", "1")
        .env("ZIPP_JIT_THRESHOLD", "1")
        .env("ZIPP_NO_POLY_LEAF_INLINE", "1")
        .env_remove("ZIPP_NOJIT")
        .env_remove("ZIPP_NO_CROSSCALL")
        .env_remove("ZIPP_NO_SAME_PROTO_CROSS2")
        .output()
        .expect("spawn cross-call fallback child");
    assert!(
        cross.status.success(),
        "cross-call fallback child failed:\n{}\n{}",
        String::from_utf8_lossy(&cross.stdout),
        String::from_utf8_lossy(&cross.stderr)
    );
}

#[test]
fn tierc_closure_all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test binary path");
    let modes: &[(&str, &[(&str, &str)])] = &[
        ("tierc", &[("ZIPP_JIT_THRESHOLD", "1")]),
        ("no-tierc-upval", &[("ZIPP_NO_TIERC_UPVAL", "1")]),
        ("mono-only", &[("ZIPP_NO_POLY_CROSSCALL", "1")]),
        (
            "generic-same-proto-cross",
            &[("ZIPP_NO_SAME_PROTO_CROSS2", "1")],
        ),
        ("no-crosscall", &[("ZIPP_NO_CROSSCALL", "1")]),
        ("no-upval-forward", &[("ZIPP_NO_TIERC_UPVAL_FORWARD", "1")]),
        ("no-upval-inc", &[("ZIPP_NO_TIERC_UPVAL_INC_I32", "1")]),
        (
            "no-upval-xorshift",
            &[("ZIPP_NO_TIERC_UPVAL_XORSHIFT", "1")],
        ),
        (
            "no-global-xorshift",
            &[("ZIPP_NO_TIERC_GLOBAL_XORSHIFT", "1")],
        ),
        ("no-poly-leaf-inline", &[("ZIPP_NO_POLY_LEAF_INLINE", "1")]),
        (
            "gc-stress",
            &[("ZIPP_GC_STRESS", "1"), ("ZIPP_JIT_THRESHOLD", "1")],
        ),
        ("interpreter", &[("ZIPP_NOJIT", "1")]),
    ];
    for (name, env) in modes {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["tierc_closure_semantics_", "--nocapture"]);
        for key in [
            "ZIPP_NOJIT",
            "ZIPP_NO_TIERC_UPVAL",
            "ZIPP_NO_POLY_CROSSCALL",
            "ZIPP_NO_SAME_PROTO_CROSS2",
            "ZIPP_NO_CROSSCALL",
            "ZIPP_NO_TIERC_UPVAL_FORWARD",
            "ZIPP_NO_TIERC_UPVAL_INC_I32",
            "ZIPP_NO_TIERC_UPVAL_XORSHIFT",
            "ZIPP_NO_TIERC_GLOBAL_XORSHIFT",
            "ZIPP_NO_POLY_LEAF_INLINE",
            "ZIPP_GC_STRESS",
            "ZIPP_JIT_THRESHOLD",
        ] {
            cmd.env_remove(key);
        }
        for (key, value) in *env {
            cmd.env(key, value);
        }
        let out = cmd.output().expect("spawn mode child");
        assert!(
            out.status.success(),
            "{name} mode failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !String::from_utf8_lossy(&out.stdout).contains("running 0 tests"),
            "{name} filter matched no semantic tests"
        );
    }
}
