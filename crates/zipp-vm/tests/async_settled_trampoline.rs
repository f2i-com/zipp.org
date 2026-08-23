//! FIFO, rejection and engagement coverage for settled-await job collapsing.

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
fn settled_trampoline_child() {
    if std::env::var_os("ZIPP_ASYNC_TRAMPOLINE_CHILD").is_none() {
        return;
    }
    let out = run_ok(
        r#"
        "use strict";
        var order = [];
        var one = Promise.resolve(1);
        async function f() {
          order.push("start");
          var s = await one;
          order.push("a");
          Promise.resolve().then(function () { order.push("queued"); });
          s += await one;
          order.push("b");
          for (var i = 0; i < 3000; i++) s += await one;
          try { await Promise.reject("x"); }
          catch (e) { order.push("caught:" + e); }
          order.push("end");
          return s;
        }
        var result = f();
        order.push("after-call");
        result.then(function (v) {
          console.log(order.join(",") + "|" + v);
        });
        "#,
    );
    // node v24.12.0: the initial call must yield, and an older queued reaction
    // must run before the await placed behind it.
    assert_eq!(out, ["start,after-call,a,queued,b,caught:x,end|3002"]);
    let hits = zipp_vm::async_inline_await_stats();
    if std::env::var_os("ZIPP_NO_ASYNC_SETTLED_TRAMPOLINE").is_some() {
        assert_eq!(hits, 0, "off switch must restore one job per await");
    } else {
        assert!(
            hits > 2500,
            "settled-await trampoline did not engage: {hits}"
        );
    }
}

#[test]
fn settled_trampoline_parity_off_and_gc_stress() {
    if std::env::var_os("ZIPP_ASYNC_TRAMPOLINE_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for mode in ["on", "off", "gc"] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "settled_trampoline_child", "--nocapture"])
            .env("ZIPP_ASYNC_TRAMPOLINE_CHILD", "1")
            .env("ZIPP_ASYNCSTATS", "1");
        if mode == "off" {
            cmd.env("ZIPP_NO_ASYNC_SETTLED_TRAMPOLINE", "1");
        }
        if mode == "gc" {
            cmd.env("ZIPP_GC_STRESS", "1");
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
