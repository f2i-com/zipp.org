//! B189b: the fully-emitted same-proto cross-call lane (`emit_cross3_call`).
//!
//! The lane bakes entry/mask/fid/this policy behind cheap per-call guards and
//! falls back to the unchanged helper on any mismatch. These tests pin the
//! paths a green benchmark cannot prove:
//!
//!   * the MID-BODY BAIL completes on the interpreter over the same window
//!     (B184's completion rule) — a late shape change makes the compiled
//!     callee's property IC miss mid-body after the lane has been hot;
//!   * a THROW inside the callee unwinds through `cross3_finish`'s
//!     `CALL_THREW` protocol into the caller's catch, and the loop continues;
//!   * an ARROW callee reads its lexical `this` through the this-mirror.
//!
//! Every case runs far past the OSR/compile thresholds with SIXTEEN rotating
//! closure identities of one FuncProto, so the call site is exactly the
//! rotating shape the lane exists for, and each also runs under
//! `ZIPP_NO_CROSS3=1` semantics implicitly via the A/B latch in the fuzzer's
//! `nocross3` mode.

#![cfg(all(feature = "jit", target_arch = "x86_64"))]

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

/// Rotating arrows; a receiver's shape changes late, so the compiled callee
/// takes a property-IC miss (or a full mid-body bail) long after the emitted
/// lane went hot. The checksum must match the interpreter's exactly.
#[test]
fn cross3_mid_body_shape_change_completes() {
    let out = run_ok(
        r#"
        "use strict";
        (function main() {
          function make(k) {
            const obj = { a: k, b: 0 };
            return (x, o) => {
              // Reads through o.a keep an IC hot inside the callee body.
              return (x + o.a + obj.b) | 0;
            };
          }
          const fns = [];
          const objs = [];
          for (let i = 0; i < 16; i++) {
            fns.push(make(i));
            objs.push({ a: i });
          }
          let acc = 0;
          for (let i = 0; i < 400000; i++) {
            const f = fns[i & 15];
            acc = (acc + f(i, objs[i & 15])) | 0;
            if (i === 300000) {
              // Late shape divergence: half the receivers gain a property,
              // so the callee's inline cache misses mid-body from here on.
              for (let j = 0; j < 16; j += 2) objs[j].zz = 1;
            }
          }
          console.log("acc=" + acc);
        })();
        "#,
    );
    assert_eq!(out, vec!["acc=-1601578624".to_string()]);
}

/// A callee that throws late; the caller catches per call and keeps looping.
/// Exercises `cross3_finish`'s CALL_THREW unwind after the lane is hot.
#[test]
fn cross3_throw_unwinds_into_caller_catch() {
    let out = run_ok(
        r#"
        "use strict";
        (function main() {
          function make(k) {
            return (x, y) => {
              if (x === 350001) throw new RangeError("boom " + k);
              return (x + y + k) | 0;
            };
          }
          const fns = [];
          for (let i = 0; i < 16; i++) fns.push(make(i));
          let acc = 0, caught = 0;
          for (let i = 0; i < 400000; i++) {
            try {
              acc = (acc + fns[i & 15](i, 1)) | 0;
            } catch (e) {
              if (e instanceof RangeError) caught++;
            }
          }
          console.log(acc, caught);
        })();
        "#,
    );
    assert_eq!(out, vec!["-1601528627 1".to_string()]);
}

/// Arrows capture `this` lexically; the emitted lane installs it from the
/// heap's this-mirror. Sixteen arrows, each born under a different receiver.
#[test]
fn cross3_arrow_this_reads_the_captured_receiver() {
    let out = run_ok(
        r#"
        "use strict";
        (function main() {
          function born(id) {
            const host = {
              id: id,
              make() {
                return (x, y) => (x + y + this.id) | 0;
              },
            };
            return host.make();
          }
          const fns = [];
          for (let i = 0; i < 16; i++) fns.push(born(i * 100));
          let acc = 0;
          for (let i = 0; i < 400000; i++) {
            acc = (acc + fns[i & 15](i, 1)) | 0;
          }
          console.log("acc=" + acc);
        })();
        "#,
    );
    assert_eq!(out, vec!["acc=-1304178624".to_string()]);
}
