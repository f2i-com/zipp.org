//! Allocation-free resolve-element records for the guarded intrinsic
//! `Promise.all` dense-Array lane.
//!
//! The fast lane is allowed only after the existing Promise constructor,
//! resolve, prototype, plain-promise and iterator proofs succeed. It removes an
//! internal `CombinatorResolver` heap object, not the observable FIFO reaction
//! job. These node-derived expectations pin ordering and every important
//! decline boundary in default, rollback, eager-JIT, interpreter and GC-stress
//! child processes.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

fn assert_accessor_overlay() {
    let out = run_ok(
        r#"
        "use strict";
        const values = [Promise.resolve(1)];
        let gets = 0;
        Object.defineProperty(values, "0", {
          configurable: true,
          enumerable: true,
          get: function () { gets++; return Promise.resolve(7); }
        });
        Promise.all(values).then(function (result) {
          console.log("accessor-overlay:" + result[0] + ":" + gets);
        });
        "#,
    );
    assert_eq!(out, ["accessor-overlay:7:1"]);
}

fn assert_virtual_length_uses_iterator() {
    let out = run_ok(
        r#"
        "use strict";
        let gets = 0;
        Object.defineProperty(Array.prototype, "0", {
          configurable: true,
          get: function () { gets++; throw "virtual-sentinel"; }
        });
        // This length is represented by `array_js_len`, not a multi-billion
        // element dense Vec. The ordinary Array iterator must still attempt
        // index 0 and observe its inherited getter exactly once.
        const combined = Promise.all(new Array(4294967294));
        delete Array.prototype[0];
        combined.then(
          function () { console.log("virtual:fulfilled:" + gets); },
          function (error) { console.log("virtual:rejected:" + error + ":" + gets); }
        );
        "#,
    );
    assert_eq!(out, ["virtual:rejected:virtual-sentinel:1"]);
}

#[test]
fn promise_all_accessor_overlay_child() {
    if std::env::var_os("ZIPP_PROMISE_ALL_ACCESSOR_CHILD").is_some() {
        assert_accessor_overlay();
    }
}

#[test]
fn promise_all_direct_child() {
    if std::env::var_os("ZIPP_PROMISE_ALL_DIRECT_CHILD").is_none() {
        return;
    }

    let ordinary = run_ok(
        r#"
        "use strict";
        const log = [];
        let fulfill;
        const pending = new Promise(function (resolve) { fulfill = resolve; });
        Promise.all([Promise.resolve(1), pending, 3]).then(
          function (values) { log.push("all:" + values.join(",")); },
          function (error) { log.push("bad:" + error); }
        );
        Promise.resolve().then(function () { log.push("tick-a"); });
        fulfill(2);
        Promise.resolve().then(function () { log.push("tick-b"); });
        Promise.all([]).then(function (values) { log.push("empty:" + values.length); });
        const sparse = new Array(3);
        sparse[1] = 7;
        Promise.all(sparse).then(function (values) {
          log.push("sparse:" + values.length + ":" + String(values[0]) + ":" +
                   values[1] + ":" + String(values[2]));
        });
        Promise.resolve().then(function () { return Promise.resolve(); }).then(function () {
          console.log(log.join("|"));
        });
        "#,
    );
    assert_eq!(
        ordinary,
        ["tick-a|tick-b|empty:0|all:1,2,3|sparse:3:undefined:7:undefined"]
    );

    let rejected = run_ok(
        r#"
        "use strict";
        const log = [];
        let reject;
        const pending = new Promise(function (_, r) { reject = r; });
        Promise.all([Promise.resolve(1), pending, Promise.resolve(3)]).then(
          function () { log.push("bad"); },
          function (error) { log.push("reject:" + error); }
        );
        pending.catch(function (error) { log.push("own:" + error); });
        Promise.resolve().then(function () { log.push("tick"); });
        reject("x");
        Promise.resolve().then(function () { return Promise.resolve(); }).then(function () {
          console.log(log.join("|"));
        });
        "#,
    );
    assert_eq!(rejected, ["tick|own:x|reject:x"]);

    let data_overlay = run_ok(
        r#"
        "use strict";
        const values = [Promise.resolve(1)];
        Object.defineProperty(values, "0", {
          configurable: true,
          enumerable: false,
          writable: false,
          value: Promise.resolve(9)
        });
        Promise.all(values).then(function (result) {
          console.log("data-overlay:" + result[0]);
        });
        "#,
    );
    assert_eq!(data_overlay, ["data-overlay:9"]);

    let mapped_arguments = run_ok(
        r#"
        function collect(x) {
          x = Promise.resolve(7);
          return Promise.all(arguments);
        }
        collect(Promise.resolve(1)).then(function (result) {
          console.log("mapped-arguments:" + result[0]);
        });
        "#,
    );
    assert_eq!(mapped_arguments, ["mapped-arguments:7"]);

    assert_virtual_length_uses_iterator();

    // The direct jobs, sparse demotion and the two fail-closed array exclusions
    // all run before this GC-mode return.
    // The remaining cases are admission declines; their generic paths already
    // have dedicated GC coverage in the promise suites.
    if std::env::var_os("ZIPP_GC_STRESS").is_some() {
        return;
    }

    assert_accessor_overlay();

    let inherited_hole = run_ok(
        r#"
        "use strict";
        let gets = 0;
        Object.defineProperty(Array.prototype, "0", {
          configurable: true,
          get: function () { gets++; return 41; }
        });
        const values = new Array(2);
        values[1] = 2;
        Promise.all(values).then(function (result) {
          delete Array.prototype[0];
          console.log("inherited:" + result.join(",") + ":" + gets);
        });
        "#,
    );
    assert_eq!(inherited_hole, ["inherited:41,2:1"]);

    let patched = run_ok(
        r#"
        "use strict";
        const originalThen = Promise.prototype.then;
        let thenReads = 0;
        Promise.prototype.then = function (onFulfilled, onRejected) {
          thenReads++;
          return originalThen.call(this, onFulfilled, onRejected);
        };
        Promise.all([Promise.resolve(1), Promise.resolve(2)]).then(function (values) {
          Promise.prototype.then = originalThen;
          console.log("patched:" + values.join(",") + ":" + thenReads);
        });
        "#,
    );
    assert_eq!(patched, ["patched:1,2:3"]);

    let subclass = run_ok(
        r#"
        "use strict";
        class P extends Promise {}
        P.all([P.resolve(3), 4]).then(function (values) {
          console.log("sub:" + (values instanceof Array) + ":" + values.join(","));
        });
        "#,
    );
    assert_eq!(subclass, ["sub:true:3,4"]);

    let thenable = run_ok(
        r#"
        "use strict";
        let calls = 0;
        const value = { then: function (resolve) { calls++; resolve(5); } };
        Promise.all([value, 6]).then(function (values) {
          console.log("thenable:" + values.join(",") + ":" + calls);
        });
        "#,
    );
    assert_eq!(thenable, ["thenable:5,6:1"]);

    let custom_iter = run_ok(
        r#"
        "use strict";
        const values = [1, 2];
        values[Symbol.iterator] = function* () { yield 8; yield 9; };
        Promise.all(values).then(function (result) {
          console.log("iter:" + result.join(","));
        });
        "#,
    );
    assert_eq!(custom_iter, ["iter:8,9"]);

    let growing = run_ok(
        r#"
        "use strict";
        const values = [10, 11];
        const originalResolve = Promise.resolve;
        Promise.resolve = function (value) {
          if (value === 10) values.push(12);
          return originalResolve.call(this, value);
        };
        Promise.all(values).then(function (result) {
          Promise.resolve = originalResolve;
          console.log("grown:" + result.join(","));
        });
        "#,
    );
    assert_eq!(growing, ["grown:10,11,12"]);
}

#[test]
fn promise_all_direct_modes_match() {
    if std::env::var_os("ZIPP_PROMISE_ALL_DIRECT_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for mode in ["default", "off", "eager", "nojit", "gc"] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "promise_all_direct_child", "--nocapture"])
            .env("ZIPP_PROMISE_ALL_DIRECT_CHILD", "1");
        match mode {
            "off" => {
                cmd.env("ZIPP_NO_PROMISE_ALL_DIRECT", "1");
            }
            "eager" => {
                cmd.env("ZIPP_JIT_THRESHOLD", "1");
            }
            "nojit" => {
                cmd.env("ZIPP_NOJIT", "1");
            }
            "gc" => {
                cmd.env("ZIPP_GC_STRESS", "1");
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
        if mode == "gc" {
            let accessor = std::process::Command::new(&exe)
                .args([
                    "--exact",
                    "promise_all_accessor_overlay_child",
                    "--nocapture",
                ])
                .env("ZIPP_PROMISE_ALL_ACCESSOR_CHILD", "1")
                .env("ZIPP_GC_STRESS", "1")
                .output()
                .expect("run isolated accessor-overlay child");
            assert!(
                accessor.status.success(),
                "isolated accessor overlay under GC stress failed:\n{}\n{}",
                String::from_utf8_lossy(&accessor.stdout),
                String::from_utf8_lossy(&accessor.stderr)
            );
        }
    }
}

#[cfg(feature = "instrument")]
#[test]
fn promise_all_direct_exact_meter_child() {
    use zipp_vm::embed;

    if std::env::var_os("ZIPP_PROMISE_ALL_DIRECT_METER_CHILD").is_none() {
        return;
    }
    let mut state = embed::compile_script(
        r#"
        let total = 0;
        async function main() {
          for (let round = 0; round < 40; round++) {
            const values = await Promise.all([
              Promise.resolve(round), Promise.resolve(round + 1), round + 2
            ]);
            total += values[0] + values[1] + values[2];
          }
        }
        main();
        "#,
    )
    .expect("meter source compiles");
    state.set_limits(u64::MAX, None);
    state.run_init().expect("meter source runs");
    println!("PROMISE_ALL_DIRECT_STEPS={}", state.steps_used());
}

#[cfg(feature = "instrument")]
#[test]
fn promise_all_direct_keeps_exact_metering() {
    if std::env::var_os("ZIPP_PROMISE_ALL_DIRECT_METER_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    let run = |off: bool| {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args([
            "--exact",
            "promise_all_direct_exact_meter_child",
            "--nocapture",
        ])
        .env("ZIPP_PROMISE_ALL_DIRECT_METER_CHILD", "1");
        if off {
            cmd.env("ZIPP_NO_PROMISE_ALL_DIRECT", "1");
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
            .find_map(|line| line.strip_prefix("PROMISE_ALL_DIRECT_STEPS="))
            .expect("meter marker")
            .parse::<u64>()
            .expect("numeric meter")
    };
    assert_eq!(run(false), run(true), "direct lane must not change gas");
}
