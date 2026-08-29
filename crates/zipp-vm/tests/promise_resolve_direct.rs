//! Semantics and mode coverage for direct construction of an intrinsic
//! `Promise.resolve(primitive)` result in the Fulfilled state.
//!
//! The lane is deliberately narrower than JavaScript's notion of primitive:
//! it accepts only non-heap `Value`s. Strings, symbols, bigints, promises,
//! thenables, subclasses, foreign-realm promises, and anything with observable
//! constructor/prototype behavior retain the full resolution algorithm.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

#[test]
fn promise_resolve_direct_child() {
    if std::env::var_os("ZIPP_PROMISE_RESOLVE_DIRECT_CHILD").is_none() {
        return;
    }

    let primitives = run_ok(
        r#"
        "use strict";
        var log = [];
        var a = Promise.resolve(7);
        var b = Promise.resolve(7);
        log.push("sync", String(a === b),
                 String(Object.getPrototypeOf(a) === Promise.prototype),
                 String(a instanceof Promise));
        a.then(function (v) { log.push("int:" + v); });
        Promise.resolve(undefined).then(function (v) { log.push("undef:" + String(v)); });
        Promise.resolve(null).then(function (v) { log.push("null:" + String(v)); });
        Promise.resolve(true).then(function (v) { log.push("bool:" + v); });
        Promise.resolve(-0).then(function (v) { log.push("negzero:" + Object.is(v, -0)); });
        Promise.resolve(NaN).then(function (v) { log.push("nan:" + Number.isNaN(v)); });
        Promise.resolve(1.25).then(function (v) { log.push("double:" + v); });
        // These are primitive in JS but heap-backed in Value; they must remain
        // correct on the conservative full path.
        Promise.resolve("heap-string").then(function (v) { log.push(v); });
        Promise.resolve().then(function () { console.log(log.join("|")); });
        "#,
    );
    assert_eq!(
        primitives,
        ["sync|false|true|true|int:7|undef:undefined|null:null|bool:true|negzero:true|nan:true|double:1.25|heap-string"]
    );

    let promise_adoption = run_ok(
        r#"
        "use strict";
        var log = [];
        var p = Promise.resolve(3);
        var reads = 0;
        Object.defineProperty(p, "constructor", {
          configurable: true,
          get: function () { reads++; return Promise; }
        });
        log.push("identity:" + (Promise.resolve(p) === p) + ":" + reads);

        Object.defineProperty(p, "constructor", {
          configurable: true,
          get: function () { reads++; return function Other() {}; }
        });
        var adopted = Promise.resolve(p);
        log.push("fresh:" + (adopted !== p) + ":" + reads);
        adopted.then(function (v) { log.push("adopted:" + v); });
        Promise.resolve().then(function () {}).then(function () {}).then(function () {
          console.log(log.join("|"));
        });
        "#,
    );
    assert_eq!(promise_adoption, ["identity:true:1|fresh:true:2|adopted:3"]);

    let thenable_adoption = run_ok(
        r#"
        "use strict";
        var log = [];
        var gets = 0, calls = 0;
        var thenable = {};
        Object.defineProperty(thenable, "then", {
          get: function () {
            gets++;
            return function (resolve) { calls++; resolve(9); };
          }
        });
        Promise.resolve(thenable).then(function (v) {
          log.push("thenable:" + v + ":" + gets + ":" + calls);
        });
        Promise.resolve().then(function () {}).then(function () {}).then(function () {
          console.log(log.join("|"));
        });
        "#,
    );
    assert_eq!(thenable_adoption, ["thenable:9:1:1"]);

    let subclasses = run_ok(
        r#"
        "use strict";
        class P extends Promise {}
        class Q extends Promise {
          static get [Symbol.species]() { return Promise; }
        }
        var p = P.resolve(5);
        var q = Q.resolve(6);
        var qp = q.then(function (x) { return x + 1; });
        console.log((p instanceof P) + ":" + (p instanceof Promise) + ":" +
                    (q instanceof Q) + ":" + (qp instanceof Q) + ":" +
                    (qp instanceof Promise));
        Promise.all([p, qp]).then(function (v) { console.log(v.join(",")); });
        "#,
    );
    assert_eq!(subclasses, ["true:true:true:false:true", "5,7"]);

    let dynamic_identity = run_ok(
        r#"
        "use strict";
        function parameter(Promise) { return Promise.resolve(2); }
        console.log("param:" + parameter({ resolve: function (v) { return v + 40; } }));

        var original = Promise.resolve;
        Promise.resolve = function (v) { return "patched:" + v; };
        console.log(Promise.resolve(3));
        Promise.resolve = original;

        var IntrinsicPromise = Promise;
        Promise = { resolve: function (v) { return "rebound:" + v; } };
        console.log(Promise.resolve(4));
        Promise = IntrinsicPromise;

        function subclass() {
          class Promise extends globalThis.Promise {}
          var p = Promise.resolve(5);
          return (p instanceof Promise) + ":" +
                 (Object.getPrototypeOf(p) === Promise.prototype);
        }
        console.log("subclass:" + subclass());
        "#,
    );
    assert_eq!(
        dynamic_identity,
        ["param:42", "patched:3", "rebound:4", "subclass:true:true"]
    );

    let realms = run_ok(
        r#"
        "use strict";
        var g = $262.createRealm().global;
        console.log("inside:" + g.eval(
          "Object.getPrototypeOf(Promise.resolve(10)) === Promise.prototype"
        ));
        var childLocal = g.Promise.resolve(12);
        console.log("outside:" +
                    (Object.getPrototypeOf(childLocal) === g.Promise.prototype));
        var foreign = g.Promise.resolve(11);
        var local = Promise.resolve(foreign);
        console.log((local === foreign) + ":" + (local instanceof Promise) + ":" +
                    (foreign instanceof Promise));
        local.then(function (v) { console.log("realm:" + v); });
        "#,
    );
    assert_eq!(
        realms,
        [
            "inside:true",
            "outside:true",
            "false:true:false",
            "realm:11"
        ]
    );
}

#[test]
fn promise_resolve_direct_modes_match() {
    if std::env::var_os("ZIPP_PROMISE_RESOLVE_DIRECT_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for mode in ["default", "off", "jit", "nojit", "gc", "gc-off", "nojit-gc"] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "promise_resolve_direct_child", "--nocapture"])
            .env("ZIPP_PROMISE_RESOLVE_DIRECT_CHILD", "1");
        match mode {
            "off" => {
                cmd.env("ZIPP_NO_PROMISE_RESOLVE_DIRECT", "1");
            }
            "jit" => {
                cmd.env("ZIPP_JIT_THRESHOLD", "1");
            }
            "nojit" => {
                cmd.env("ZIPP_NOJIT", "1");
            }
            "gc" => {
                cmd.env("ZIPP_GC_STRESS", "1");
            }
            "gc-off" => {
                cmd.env("ZIPP_GC_STRESS", "1")
                    .env("ZIPP_NO_PROMISE_RESOLVE_DIRECT", "1");
            }
            "nojit-gc" => {
                cmd.env("ZIPP_NOJIT", "1").env("ZIPP_GC_STRESS", "1");
            }
            _ => {}
        }
        let out = cmd.output().expect("re-run test binary");
        assert!(
            out.status.success(),
            "mode {mode} failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[cfg(feature = "instrument")]
#[test]
fn promise_resolve_direct_meter_child() {
    use zipp_vm::embed;

    if std::env::var_os("ZIPP_PROMISE_RESOLVE_DIRECT_METER_CHILD").is_none() {
        return;
    }
    let mut state = embed::compile_script(
        r#"
        var sum = 0;
        for (var i = 0; i < 2000; i++) {
          Promise.resolve(i).then(function (v) { sum += v; });
        }
        Promise.resolve().then(function () { console.log(sum); });
        "#,
    )
    .expect("meter source compiles");
    state.set_limits(u64::MAX, None);
    state.run_init().expect("meter source runs");
    assert_eq!(state.take_output(), ["1999000"]);
    println!("PROMISE_RESOLVE_DIRECT_STEPS={}", state.steps_used());
}

#[cfg(feature = "instrument")]
#[test]
fn promise_resolve_direct_keeps_exact_metering() {
    if std::env::var_os("ZIPP_PROMISE_RESOLVE_DIRECT_METER_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    let run = |off: bool| {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args([
            "--exact",
            "promise_resolve_direct_meter_child",
            "--nocapture",
        ])
        .env("ZIPP_PROMISE_RESOLVE_DIRECT_METER_CHILD", "1");
        if off {
            cmd.env("ZIPP_NO_PROMISE_RESOLVE_DIRECT", "1");
        }
        let output = cmd.output().expect("run meter child");
        assert!(
            output.status.success(),
            "meter child failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .find_map(|line| line.strip_prefix("PROMISE_RESOLVE_DIRECT_STEPS="))
            .expect("meter marker")
            .parse::<u64>()
            .expect("numeric meter")
    };
    assert_eq!(run(false), run(true), "direct lane must not change gas");
}
