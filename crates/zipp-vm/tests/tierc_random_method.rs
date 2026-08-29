//! Exactness coverage for Tier-C dispatch of a captured, dynamically resolved
//! zero-argument `random` method. The name is intentionally not treated as a
//! pristine Math intrinsic: replacements, receiver `this`, arrows, accessors
//! and throws remain live.

use std::process::Command;

use zipp_vm::run;

const SOURCE: &str = r#"
    "use strict";
    function one() {
        this.calls = (this.calls + 1) | 0;
        return (this.base + this.calls) | 0;
    }
    function two() {
        this.calls = (this.calls + 2) | 0;
        return (this.base - this.calls) | 0;
    }
    const lexical = 77;
    const arrow = () => lexical;
    function rotate(o, n) {
        let sum = 0;
        for (let i = 0; i < n; i++) {
            if (i === 2000) o.random = two;
            if (i === 4000) o.random = arrow;
            sum = (sum + o.random()) | 0;
        }
        return sum;
    }
    const receiver = { base: 10, calls: 0, random: one };
    console.log("rotate", rotate(receiver, 6000), receiver.calls);

    let getterCalls = 0;
    const accessor = { base: 9 };
    Object.defineProperty(accessor, "random", {
        get: function () {
            getterCalls++;
            return function () { return this.base; };
        }
    });
    function throughAccessor(o, n) {
        let sum = 0;
        for (let i = 0; i < n; i++) sum += o.random();
        return sum;
    }
    console.log("accessor", throughAccessor(accessor, 120), getterCalls);

    let throwCalls = 0;
    function throwsOnce() {
        throwCalls++;
        if (throwCalls === 100) throw new Error("stop");
        return 1;
    }
    function untilThrow(o) {
        let sum = 0;
        for (let i = 0; i < 1000; i++) sum += o.random();
        return sum;
    }
    let thrown = "";
    try { untilThrow({ random: throwsOnce }); } catch (e) { thrown = e.message; }
    console.log("throw", thrown, throwCalls);

    // One hot CallMethod site sees distinct receivers with the same hidden
    // class.  The direct prefix must read each receiver's LIVE slot rather than
    // baking either the callee identity or the first object's value.
    function ownOne() { return (this.tag + 1) | 0; }
    function ownTwo() { return (this.tag * 2) | 0; }
    function makeReceiver(tag, fn) { return { tag: tag, random: fn }; }
    function invoke(o) { return o.random(); }
    const sameA = makeReceiver(10, ownOne);
    const sameB = makeReceiver(7, ownTwo);
    let sameSum = 0;
    for (let i = 0; i < 4000; i++) {
        sameSum = (sameSum + invoke((i & 1) ? sameB : sameA)) | 0;
    }
    // Same-slot replacement keeps the shape but must change the live target.
    sameA.random = ownTwo;
    const replaced = invoke(sameA);
    // Delete/re-add changes layout state.  The missing call must throw once,
    // and the re-added method must not inherit any stale IC/callee identity.
    delete sameA.random;
    let deleteThrew = false;
    try { invoke(sameA); } catch (e) { deleteThrew = e instanceof TypeError; }
    sameA.random = ownOne;
    const restored = invoke(sameA);
    // Data -> accessor replacement must execute the getter exactly once.
    let transitionGets = 0;
    Object.defineProperty(sameB, "random", {
        configurable: true,
        get: function () { transitionGets++; return ownOne; }
    });
    const transitioned = invoke(sameB);
    console.log("own", sameSum, replaced, deleteThrew, restored, transitioned, transitionGets);

    // Proxies/exotics never use a cached raw slot. Every method lookup must
    // still hit the trap, while OrdinaryCallBindThis receives the Proxy.
    let proxyGets = 0;
    const proxied = new Proxy(makeReceiver(5, ownOne), {
        get: function (target, key) {
            if (key === "random") proxyGets++;
            return target[key];
        }
    });
    let proxySum = 0;
    for (let i = 0; i < 120; i++) proxySum += invoke(proxied);
    console.log("proxy", proxySum, proxyGets);

    // Bound/native/non-callable targets are intentionally outside the direct
    // lane and retain the generic call protocol.
    const bound = ownOne.bind({ tag: 40 });
    const boundResult = invoke(makeReceiver(1, bound));
    const nativeResult = invoke(makeReceiver(0, Math.abs));
    let nonCallableThrew = false;
    try { invoke(makeReceiver(0, 17)); }
    catch (e) { nonCallableThrew = e instanceof TypeError; }
    console.log("other", boundResult, Number.isNaN(nativeResult), nonCallableThrew);

    // A child-realm body must resolve its intrinsics in that realm even when
    // called as a method of a main-realm object.
    const realm = $262.createRealm().global;
    realm.eval("function realmRandom(){ return Object === this.expected ? this.tag : -500; }");
    const realmRecv = { tag: 73, expected: realm.Object, random: realm.realmRandom };
    let realmSum = 0;
    for (let i = 0; i < 2000; i++) realmSum += invoke(realmRecv);
    console.log("realm", realmSum);

    // A direct-eval function carries an EvalScope. It may be ineligible for a
    // Tier-C cross entry, but must always fall back without losing the scope.
    function makeEvalRandom() {
        let secret = 31;
        return eval("(function(){ return (secret + this.tag) | 0; })");
    }
    const evalRecv = makeReceiver(7, makeEvalRandom());
    let evalSum = 0;
    for (let i = 0; i < 2000; i++) evalSum += invoke(evalRecv);
    console.log("eval", evalSum);

    // With ZIPP_GC_STRESS this allocation between calls exercises collection,
    // free-slot reuse and stale-IC/ABA resistance while the direct call site is
    // hot. The receiver itself remains a true JS root throughout.
    const gcRecv = makeReceiver(8, ownOne);
    let gcSum = 0;
    for (let i = 0; i < 500; i++) {
        const trash = { i: i, text: "gc" + i };
        gcSum += invoke(gcRecv) + (trash.i - i);
    }
    console.log("gc", gcSum);
"#;

fn expected() -> Vec<String> {
    vec![
        "rotate -5807000 6000".into(),
        "accessor 1080 120".into(),
        "throw stop 100".into(),
        "own 50000 20 true 11 8 1".into(),
        "proxy 720 120".into(),
        "other 41 true true".into(),
        "realm 146000".into(),
        "eval 76000".into(),
        "gc 4500".into(),
    ]
}

#[test]
fn execution_mode_child() {
    if std::env::var_os("ZIPP_RANDOM_METHOD_CHILD").is_none() {
        return;
    }
    let outcome = run(SOURCE).expect("source compiles");
    assert!(
        outcome.error.is_none(),
        "runtime error: {:?}",
        outcome.error
    );
    assert_eq!(outcome.output, expected());
}

#[test]
fn jit_method_inline_nojit_and_gc_modes_match() {
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("hot", &[("ZIPP_JIT_THRESHOLD", "1")][..]),
        (
            "no_method_inline",
            &[("ZIPP_JIT_THRESHOLD", "1"), ("ZIPP_NO_METHOD_INLINE", "1")][..],
        ),
        ("nojit", &[("ZIPP_NOJIT", "1")][..]),
        (
            "hot_gc",
            &[("ZIPP_JIT_THRESHOLD", "1"), ("ZIPP_GC_STRESS", "1")][..],
        ),
    ] {
        let mut cmd = Command::new(&exe);
        cmd.args(["execution_mode_child", "--exact"])
            .env("ZIPP_RANDOM_METHOD_CHILD", "1")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env_remove("ZIPP_NO_METHOD_INLINE")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_GC_STRESS");
        for &(key, value) in env {
            cmd.env(key, value);
        }
        let out = cmd.output().expect("spawn mode child");
        assert!(
            out.status.success()
                && !String::from_utf8_lossy(&out.stdout).contains("running 0 tests"),
            "{mode} child failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[test]
fn capture_first_member_calls_reach_tier_c() {
    let exe = std::env::current_exe().expect("test binary path");
    let out = Command::new(exe)
        .args(["execution_mode_child", "--exact", "--nocapture"])
        .env("ZIPP_RANDOM_METHOD_CHILD", "1")
        .env("ZIPP_JIT_THRESHOLD", "1")
        .env("ZIPP_JITLOG", "1")
        .env("ZIPP_VM_DUMP", "1")
        .env_remove("ZIPP_NOJIT")
        .env_remove("ZIPP_NO_METHOD_INLINE")
        .output()
        .expect("spawn engagement child");
    assert!(
        out.status.success(),
        "engagement child failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("CallWithThis {") && !stderr.contains("CallMethod {"),
        "member calls were not lowered to capture-first CallWithThis bytecode:\n{stderr}"
    );
    assert!(
        stderr.contains("Tier C fn"),
        "capture-first member-call bodies did not reach Tier C:\n{stderr}"
    );
}
