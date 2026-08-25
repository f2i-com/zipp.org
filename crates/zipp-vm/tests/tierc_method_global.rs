//! Transactional Tier-C own-method/global splice coverage. The optimized arm
//! may read and update directly-routable root globals, but it must validate the
//! live receiver/callee/routes/types/depth before its first physical write.

use std::process::Command;

use zipp_vm::run;

const SOURCE: &str = r#"
    let randomState = 123456789;
    function seededRandom() {
        randomState ^= randomState << 13;
        randomState ^= randomState >>> 17;
        randomState ^= randomState << 5;
        return (randomState >>> 0) / 4294967296;
    }
    const main = { random: seededRandom };
    function invoke(o) { return o.random(); }

    let checksum = 0;
    for (let i = 0; i < 4096; i++) {
        checksum = (checksum + ((invoke(main) * 1000000) | 0)) | 0;
    }
    console.log("stable", checksum, randomState);

    // The first two textual stores must remain buffered while txB's later tag
    // check can still decline. Fallback then executes the real call exactly
    // once: no partial/double commit is observable.
    let txA = 1;
    let txB = 2;
    let txMirror = 0;
    function transactional() {
        txA = (txA + 1) | 0;
        txA = (txA ^ 3) | 0;
        txMirror = txA;
        return txA + txB;
    }
    const tx = { random: transactional };
    function invokeTx(o) { return o.random(); }
    for (let i = 0; i < 64; i++) invokeTx(tx);
    txA = 10;
    txB = 20;
    txMirror = 0;
    txB = "x";
    console.log("late", invokeTx(tx), txA, txMirror);

    // Concrete persistent-script slot reuse: the first sloppy evalScript
    // initializes a referenced-but-undeclared main-program slot without adding
    // a var/lexical declaration registry entry. A later script GDI can therefore
    // register a const on that same live slot. Its initializer correctly throws
    // in this VM model because the slot is already initialized; registration is
    // nevertheless persistent, so the already-compiled method's next store must
    // fall back and throw without changing the value.
    $262.evalScript("lateReuse = 10;");
    function lateRandom() {
        lateReuse = (lateReuse + 1) | 0;
        return lateReuse;
    }
    const lateObj = { random: lateRandom };
    function invokeLate(o) { return o.random(); }
    for (let i = 0; i < 64; i++) invokeLate(lateObj);
    const lateBefore = lateReuse;
    let declarationThrew = false;
    try { $262.evalScript("const lateReuse = 500;"); }
    catch (e) { declarationThrew = e instanceof TypeError; }
    let lateCallThrew = false;
    try { invokeLate(lateObj); }
    catch (e) { lateCallThrew = e instanceof TypeError; }
    console.log("evalconst", declarationThrew, lateCallThrew, lateBefore, lateReuse);

    // Same-shape value replacement must be read live; deletion/accessor
    // transitions must take the full property/call semantics.
    let replacementCalls = 0;
    function replacement() {
        replacementCalls = (replacementCalls + 1) | 0;
        return 0.25;
    }
    main.random = replacement;
    console.log("replace", invoke(main), replacementCalls);
    delete main.random;
    let deleteThrew = false;
    try { invoke(main); }
    catch (e) { deleteThrew = e instanceof TypeError; }
    let accessorGets = 0;
    Object.defineProperty(main, "random", {
        configurable: true,
        get: function () { accessorGets++; return replacement; }
    });
    console.log("transition", deleteThrew, invoke(main), accessorGets, replacementCalls);

    // Identity and descriptor checks must not collapse same-shape receivers or
    // proxies into the baked arm.
    function three() { return 3; }
    const sameShape = { random: three };
    console.log("same", invoke(sameShape));
    let proxyGets = 0;
    const proxied = new Proxy({ random: replacement }, {
        get: function (target, key) {
            if (key === "random") proxyGets++;
            return target[key];
        }
    });
    console.log("proxy", invoke(proxied), proxyGets, replacementCalls);

    // Bound/native/non-callable and lexical/captureful functions remain on the
    // generic call protocol.
    function readV() { return this.v; }
    const bound = readV.bind({ v: 41 });
    let nonCallableThrew = false;
    try { invoke({ random: 17 }); }
    catch (e) { nonCallableThrew = e instanceof TypeError; }
    const lexical = 17;
    const arrow = () => lexical;
    function makeClosure() {
        let n = 5;
        return function () { n = (n + 1) | 0; return n; };
    }
    const captured = makeClosure();
    function withDefault(x = 9) { return x; }
    console.log(
        "other",
        invoke({ random: bound }),
        Number.isNaN(invoke({ random: Math.abs })),
        nonCallableThrew,
        invoke({ random: arrow }),
        invoke({ random: captured }),
        invoke({ random: captured }),
        invoke({ random: withDefault })
    );

    // Child-realm and direct-eval callables own different global/environment
    // routes and must not consume the root-global splice.
    const realm = $262.createRealm().global;
    realm.eval("var realmState=40; function realmRandom(){ realmState=(realmState+1)|0; return realmState; }");
    const realmObj = { random: realm.realmRandom };
    console.log("realm", invoke(realmObj), invoke(realmObj));
    function makeEval() {
        let secret = 31;
        return eval("(function(){ secret=(secret+1)|0; return secret; })");
    }
    const evalObj = { random: makeEval() };
    console.log("eval", invoke(evalObj), invoke(evalObj));

    // A const-target store is never admitted. The real call's earlier mutable
    // store remains observable before its TypeError, exactly once.
    let beforeConst = 0;
    const locked = 5;
    function constStore() {
        beforeConst = (beforeConst + 1) | 0;
        locked = 6;
        return 0;
    }
    let constThrew = false;
    try { invoke({ random: constStore }); }
    catch (e) { constThrew = e instanceof TypeError; }
    console.log("const", constThrew, beforeConst, locked);

    // Under ZIPP_GC_STRESS, allocations between calls exercise receiver/callee
    // rooting, slot-version ABA guards and stale-plan fallback.
    const gcObj = { random: three };
    let gcSum = 0;
    for (let i = 0; i < 400; i++) {
        const trash = { i: i, s: "g" + i };
        gcSum += invoke(gcObj) + (trash.i - i);
    }
    console.log("gc", gcSum);
"#;

fn expected() -> Vec<String> {
    vec![
        "stable 2057774018 1120112305".into(),
        "late 8x 8 8".into(),
        "evalconst true true 74 74".into(),
        "replace 0.25 1".into(),
        "transition true 0.25 1 2".into(),
        "same 3".into(),
        "proxy 0.25 1 3".into(),
        "other 41 true true 17 6 7 9".into(),
        "realm 41 42".into(),
        "eval 32 33".into(),
        "const true 1 5".into(),
        "gc 1200".into(),
    ]
}

#[test]
fn execution_mode_child() {
    if std::env::var_os("ZIPP_METHOD_GLOBAL_CHILD").is_none() {
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
fn transactional_lane_matches_fallback_nojit_and_gc() {
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("hot", &[("ZIPP_JIT_THRESHOLD", "2")][..]),
        (
            "fallback",
            &[
                ("ZIPP_JIT_THRESHOLD", "2"),
                ("ZIPP_NO_TIERC_METHOD_GLOBAL_INLINE", "1"),
            ][..],
        ),
        ("nojit", &[("ZIPP_NOJIT", "1")][..]),
        (
            "gc",
            &[("ZIPP_JIT_THRESHOLD", "2"), ("ZIPP_GC_STRESS", "1")][..],
        ),
    ] {
        let mut cmd = Command::new(&exe);
        cmd.args(["execution_mode_child", "--exact"])
            .env("ZIPP_METHOD_GLOBAL_CHILD", "1")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env_remove("ZIPP_NO_TIERC_METHOD_GLOBAL_INLINE")
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
fn transactional_method_global_lane_engages() {
    let exe = std::env::current_exe().expect("test binary path");
    let out = Command::new(exe)
        .args(["execution_mode_child", "--exact", "--nocapture"])
        .env("ZIPP_METHOD_GLOBAL_CHILD", "1")
        .env("ZIPP_JIT_THRESHOLD", "2")
        .env("ZIPP_JITLOG", "1")
        .env_remove("ZIPP_NO_TIERC_METHOD_GLOBAL_INLINE")
        .env_remove("ZIPP_NOJIT")
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
        stderr.contains("method-global=1") && stderr.contains("method LANE"),
        "transactional method/global lane was not emitted:\n{stderr}"
    );
}
