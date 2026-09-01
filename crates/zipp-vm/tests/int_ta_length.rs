//! INTEGER-tier TypedArray `.length` snapshots.
//!
//! A TA length pin is deliberately distinct from an element pin: it is created
//! only for `GetProp "length"`, re-proves the live inherited intrinsic getter
//! at every native entry, and carries only the exact effective length.  All
//! property/prototype mutations, detach/OOB states, and concurrently growable
//! shared length-tracking views fail closed to the ordinary property path.

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

/// Every existing non-BigInt fixed-width kind can use the length-only marker,
/// including Uint32 and floats; that does not admit their elements. BigInt is
/// an explicit generic-path control and must remain correct.
#[test]
fn ta_len_parity_kind_matrix() {
    assert_matches_node(
        r#"
        "use strict";
        function k0(a,n){let s=0;for(let i=0;i<n;i++)s=(Math.imul(s^a.length,33)^i)|0;return s}
        function k1(a,n){let s=0;for(let i=0;i<n;i++)s=(Math.imul(s^a.length,33)^i)|0;return s}
        function k2(a,n){let s=0;for(let i=0;i<n;i++)s=(Math.imul(s^a.length,33)^i)|0;return s}
        function k3(a,n){let s=0;for(let i=0;i<n;i++)s=(Math.imul(s^a.length,33)^i)|0;return s}
        function k4(a,n){let s=0;for(let i=0;i<n;i++)s=(Math.imul(s^a.length,33)^i)|0;return s}
        function k5(a,n){let s=0;for(let i=0;i<n;i++)s=(Math.imul(s^a.length,33)^i)|0;return s}
        function k6(a,n){let s=0;for(let i=0;i<n;i++)s=(Math.imul(s^a.length,33)^i)|0;return s}
        function k7(a,n){let s=0;for(let i=0;i<n;i++)s=(Math.imul(s^a.length,33)^i)|0;return s}
        function k8(a,n){let s=0;for(let i=0;i<n;i++)s=(Math.imul(s^a.length,33)^i)|0;return s}
        function kb0(a,n){let s=0;for(let i=0;i<n;i++)s=(Math.imul(s^a.length,33)^i)|0;return s}
        function kb1(a,n){let s=0;for(let i=0;i<n;i++)s=(Math.imul(s^a.length,33)^i)|0;return s}
        console.log(
          k0(new Int8Array(3),2400),
          k1(new Uint8Array(5),2400),
          k2(new Uint8ClampedArray(7),2400),
          k3(new Int16Array(9),2400),
          k4(new Uint16Array(11),2400),
          k5(new Int32Array(13),2400),
          k6(new Uint32Array(15),2400),
          k7(new Float32Array(17),2400),
          k8(new Float64Array(19),2400),
          kb0(new BigInt64Array(21),2400),
          kb1(new BigUint64Array(23),2400)
        );
        "#,
    );
}

/// Warm the intrinsic pin first, then replace every observable layer that can
/// win over `%TypedArray%.prototype.length`. Each getter must run exactly once
/// per source-level read; restoring the pristine chain must make the real
/// effective length visible again.
#[test]
fn ta_len_parity_own_accessor_and_prototype_overrides() {
    assert_matches_node(
        r#"
        "use strict";
        let ta = new Uint8Array(6), hits = 0;
        function hot(n) {
          let s = 0;
          for (let i = 0; i < n; i++) s = (Math.imul(s ^ ta.length, 33) ^ i) | 0;
          return s;
        }
        const warm = hot(2400);

        Object.defineProperty(ta, "length", {value: 17, configurable: true});
        const ownData = hot(211);
        delete ta.length;

        Object.defineProperty(ta, "length", {
          get() { hits++; return 9; }, configurable: true
        });
        const ownAccessor = hot(213), ownHits = hits;
        delete ta.length;

        Object.defineProperty(Uint8Array.prototype, "length", {
          get() { hits++; return 11; }, configurable: true
        });
        const kindProto = hot(215), kindHits = hits - ownHits;
        delete Uint8Array.prototype.length;

        const base = Object.getPrototypeOf(Uint8Array.prototype);
        const inserted = Object.create(base), beforeRelink = hits;
        Object.defineProperty(inserted, "length", {
          get() { hits++; return 12; }, configurable: true
        });
        Object.setPrototypeOf(Uint8Array.prototype, inserted);
        const relinkedKindProto = hot(216), relinkHits = hits - beforeRelink;
        Object.setPrototypeOf(Uint8Array.prototype, base);

        const saved = Object.getOwnPropertyDescriptor(base, "length");
        const beforeBase = hits;
        Object.defineProperty(base, "length", {
          get() { hits++; return 13; }, configurable: true
        });
        const baseProto = hot(217), baseHits = hits - beforeBase;
        Object.defineProperty(base, "length", saved);

        const custom = Object.create(Uint8Array.prototype);
        Object.defineProperty(custom, "length", {
          get() { hits++; return 15; }, configurable: true
        });
        Object.setPrototypeOf(ta, custom);
        const customProto = hot(219);
        Object.setPrototypeOf(ta, Uint8Array.prototype);
        const restored = hot(221);
        console.log(
          warm, ownData, ownAccessor, ownHits,
          kindProto, kindHits, relinkedKindProto, relinkHits, baseProto, baseHits,
          customProto, hits, restored, ta.length
        );
        "#,
    );
}

/// Prototype lookup cannot use a small fixed-depth shortcut.  A legal custom
/// chain deeper than eight objects must still find its nearest `length` getter
/// instead of falling through to the TypedArray's internal slot.
#[test]
fn ta_len_parity_deep_custom_prototype_chain() {
    assert_matches_node(
        r#"
        "use strict";
        let ta = new Uint8Array(6), hits = 0;
        function hot(n) {
          let s = 0;
          for (let i = 0; i < n; i++) s = (Math.imul(s ^ ta.length, 33) ^ i) | 0;
          return s;
        }
        const warm = hot(2400);
        let deep = Object.create(Uint8Array.prototype);
        Object.defineProperty(deep, "length", {
          get() { hits++; return 29; }, configurable: true
        });
        for (let i = 0; i < 12; i++) deep = Object.create(deep);
        Object.setPrototypeOf(ta, deep);
        const shadowed = hot(223), shadowHits = hits;
        Object.setPrototypeOf(ta, Uint8Array.prototype);
        const restored = hot(227);
        console.log(warm, shadowed, shadowHits, restored, ta.length);
        "#,
    );
}

/// Detach and every resizable-buffer state are sampled at a fresh native entry.
/// Fixed views report zero while OOB and recover their fixed length after grow;
/// local length-tracking views follow the live byte length exactly.
#[test]
fn ta_len_parity_detach_and_resizable_buffers() {
    assert_matches_node(
        r#"
        "use strict";
        function fixedLen(a,n){let s=0;for(let i=0;i<n;i++)s=(Math.imul(s^a.length,33)^i)|0;return s}
        function trackLen(a,n){let s=0;for(let i=0;i<n;i++)s=(s^a.length^i)|0;return s}

        let db = new ArrayBuffer(16), detached = new Uint8Array(db);
        const d0 = fixedLen(detached,2400);
        db.transfer();
        const d1 = fixedLen(detached,333);

        let rb = new ArrayBuffer(24,{maxByteLength:64});
        let fixed = new Uint16Array(rb,4,6);
        let tracking = new Uint16Array(rb,4);
        const a0 = fixedLen(fixed,2400) + "/" + trackLen(tracking,2400);
        rb.resize(10);
        const a1 = fixedLen(fixed,333) + "/" + trackLen(tracking,2400);
        rb.resize(40);
        const a2 = fixedLen(fixed,2400) + "/" + trackLen(tracking,2400);
        rb.resize(4);
        const a3 = fixedLen(fixed,333) + "/" + trackLen(tracking,333);
        console.log(d0,d1,a0,a1,a2,a3,detached.length,fixed.length,tracking.length);
        "#,
    );
}

/// A fixed GSAB view has an immutable reported length and is safe to pin. A
/// length-tracking GSAB can grow concurrently, so the planner/snapshot must
/// decline it; sequential grows here verify the generic fallback's answer.
#[test]
fn ta_len_parity_growable_shared_buffer() {
    assert_matches_node(
        r#"
        "use strict";
        function fixedShared(a,n){let s=0;for(let i=0;i<n;i++)s=(Math.imul(s^a.length,33)^i)|0;return s}
        function trackingShared(a,n){let s=0;for(let i=0;i<n;i++)s=(s^a.length^i)|0;return s}
        let b = new SharedArrayBuffer(8,{maxByteLength:64});
        let fixed = new Uint8Array(b,0,4), tracking = new Uint8Array(b);
        const a = fixedShared(fixed,2400) + "/" + trackingShared(tracking,2400);
        b.grow(32);
        const c = fixedShared(fixed,2400) + "/" + trackingShared(tracking,2400);
        console.log(a,c,fixed.length,tracking.length);
        "#,
    );
}

/// Child target for default/off mechanism assertions. Uint32 `.length` may use
/// the marker while Uint32 element access must continue to decline INTEGER.
#[test]
fn ta_len_mechanism_probe() {
    assert_matches_node(
        r#"
        function lengthOnly(a,n){let s=0;for(let i=0;i<n;i++)s=(Math.imul(s^a.length,33)^i)|0;return s}
        function uint32Elements(a,n){let s=0;for(let i=0;i<n;i++)s=(s+(a[i&7]|0))|0;return s}
        console.log(lengthOnly(new Uint32Array(13),4000));
        console.log(uint32Elements(new Uint32Array([0,1,2147483647,2147483648,3,4,5,6]),4000));
        "#,
    );
}

#[test]
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn ta_len_mechanism_and_uint32_element_decline() {
    let exe = std::env::current_exe().expect("test exe path");
    let run = |off: bool| {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["ta_len_mechanism_probe", "--exact", "--nocapture"])
            .env("ZIPP_JITLOG", "1")
            .env("ZIPP_JITDECLINE", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_NO_INT_TA_LENGTH");
        if off {
            cmd.env("ZIPP_NO_INT_TA_LENGTH", "1");
        }
        cmd.output().expect("spawn test binary")
    };

    let on = run(false);
    assert!(
        on.status.success(),
        "default child failed:\n{}\n{}",
        String::from_utf8_lossy(&on.stdout),
        String::from_utf8_lossy(&on.stderr)
    );
    let on_log = String::from_utf8_lossy(&on.stderr);
    assert!(
        on_log.contains("INT region") || on_log.contains("INT-GPR region"),
        "length-only probe did not engage INTEGER:\n{on_log}"
    );
    assert!(
        on_log.contains("GetIndex") && on_log.contains("int-reject"),
        "Uint32 element access was accidentally admitted:\n{on_log}"
    );

    let off = run(true);
    assert!(
        off.status.success(),
        "off child failed:\n{}\n{}",
        String::from_utf8_lossy(&off.stdout),
        String::from_utf8_lossy(&off.stderr)
    );
    let off_log = String::from_utf8_lossy(&off.stderr);
    assert!(
        off_log.contains("GetProp") && off_log.contains("int-reject"),
        "length off switch did not restore the GetProp decline:\n{off_log}"
    );
}

#[test]
fn ta_len_shared_tracking_probe() {
    assert_matches_node(
        r#"
        function k(a,n){let s=0;for(let i=0;i<n;i++)s=(Math.imul(s^a.length,33)^i)|0;return s}
        let b=new SharedArrayBuffer(8,{maxByteLength:32}), a=new Uint8Array(b);
        console.log(k(a,4000));
        "#,
    );
}

#[test]
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn ta_len_shared_tracking_is_a_planning_decline() {
    let exe = std::env::current_exe().expect("test exe path");
    let out = std::process::Command::new(exe)
        .args(["ta_len_shared_tracking_probe", "--exact", "--nocapture"])
        .env("ZIPP_JITLOG", "1")
        .env("ZIPP_JITDECLINE", "1")
        .env_remove("ZIPP_NOJIT")
        .env_remove("ZIPP_NO_INT_TA_LENGTH")
        .output()
        .expect("spawn shared-tracking probe");
    assert!(
        out.status.success(),
        "shared probe failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let log = String::from_utf8_lossy(&out.stderr);
    assert!(
        log.contains("GetProp") && log.contains("int-reject"),
        "length-tracking GSAB unexpectedly entered INTEGER:\n{log}"
    );
}

const CROSS_REALM: &str = r#"
  "use strict";
  const g=$262.createRealm().global;
  const a=g.eval("new Uint8Array(9)");
  function k(x,n){let s=0;for(let i=0;i<n;i++)s=(Math.imul(s^x.length,33)^i)|0;return s}
  console.log(k(a,4000),a.length,Object.getPrototypeOf(a)===Uint8Array.prototype);
"#;

#[test]
fn ta_len_cross_realm_probe() {
    if std::env::var_os("ZIPP_TA_LEN_CROSS_CHILD").is_some() {
        assert_eq!(run_ok(CROSS_REALM), ["-226702336 9 false"]);
    }
}

/// A child-realm instance does not inherit from this realm's intrinsic kind
/// prototype. It must stay on the ordinary property path in both JIT and NOJIT.
#[test]
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn ta_len_cross_realm_declines_and_matches_nojit() {
    let exe = std::env::current_exe().expect("test exe path");
    let run = |nojit: bool| {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["ta_len_cross_realm_probe", "--exact", "--nocapture"])
            .env("ZIPP_TA_LEN_CROSS_CHILD", "1")
            .env("ZIPP_JITDECLINE", "1")
            .env_remove("ZIPP_NOJIT");
        if nojit {
            cmd.env("ZIPP_NOJIT", "1");
        }
        cmd.output().expect("spawn cross-realm probe")
    };
    let jit = run(false);
    let nojit = run(true);
    for (name, out) in [("jit", &jit), ("nojit", &nojit)] {
        assert!(
            out.status.success(),
            "{name} child failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let log = String::from_utf8_lossy(&jit.stderr);
    assert!(
        log.contains("GetProp") && log.contains("int-reject"),
        "cross-realm TA unexpectedly received a length marker:\n{log}"
    );
}

/// Replay all Node-differential cases in fresh processes so the latched off
/// switch, eager compilation, GC stress, and forced interpreter cannot leak
/// into one another.
#[test]
fn ta_len_all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    for (name, mode) in [
        ("off", ("ZIPP_NO_INT_TA_LENGTH", "1")),
        ("eager", ("ZIPP_JIT_THRESHOLD", "1")),
        ("gc", ("ZIPP_GC_STRESS", "1")),
        ("nojit", ("ZIPP_NOJIT", "1")),
    ] {
        let out = std::process::Command::new(&exe)
            .arg("ta_len_parity_")
            .arg("--nocapture")
            .env(mode.0, mode.1)
            .output()
            .expect("spawn mode child");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{name} failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !stdout.contains("running 0 tests"),
            "mode filter matched nothing: {stdout}"
        );
    }
}

/// Sandboxed/metered execution charges exactly the same source bytecodes as
/// forced interpretation. The snapshot and native property read cannot make
/// resource limits depend on which tier happened to win.
#[test]
#[cfg(feature = "instrument")]
fn ta_len_meter_is_exact() {
    const SCRIPT: &str = r#"
      function k(a,n){let s=0;for(let i=0;i<n;i++)s=(Math.imul(s^a.length,33)^i)|0;return s}
      k(new Uint8Array(13),20000)
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
        let expr = format!("globalThis.__taLenResult=(0,eval)({SCRIPT:?});");
        state.eval_in_context(&expr).expect("kernel runs");
        let used = before - state.steps_remaining();
        let value = state
            .eval_in_context("String(globalThis.__taLenResult)")
            .expect("result reads")
            .as_str()
            .expect("string result")
            .to_owned();
        (used, value)
    }
    assert_eq!(metered(false), metered(true));
}
