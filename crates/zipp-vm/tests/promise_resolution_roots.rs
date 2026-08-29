//! Promise targets held only by an in-flight abstract operation must survive
//! re-entrant JavaScript and GC until they are published in a register, queue,
//! reaction, or other traced VM owner.

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}\nsource:\n{src}",
        out.error
    );
    out.output
}

const CHURN: &str = r#"
    function churn(tag) {
      var keep = [];
      for (var i = 0; i < 96; i++) {
        keep.push({ tag: tag, i: i, text: "cell-" + i });
      }
      return keep.length;
    }
"#;

#[test]
fn promise_resolution_roots_child() {
    if std::env::var_os("ZIPP_PROMISE_RESOLUTION_ROOTS_CHILD").is_none() {
        return;
    }

    let constructor = run_ok(&format!(
        r#"
        {CHURN}
        var p = new Promise(function () {{ churn("constructor"); }});
        console.log("constructor:" + (p instanceof Promise) + ":" +
                    (Object.getPrototypeOf(p) === Promise.prototype) + ":" +
                    (typeof p.then));
        "#
    ));
    assert_eq!(constructor, ["constructor:true:true:function"]);

    let reflected_constructor = run_ok(&format!(
        r#"
        {CHURN}
        function NewTarget() {{}}
        var newTarget = new Proxy(NewTarget, {{
          get: function (target, key) {{
            if (key === "prototype") return {{ marker: 77 }};
            return target[key];
          }}
        }});
        var p = Reflect.construct(Promise, [function () {{ churn("reflect-constructor"); }}], newTarget);
        console.log("reflect-constructor:" + Object.getPrototypeOf(p).marker + ":" +
                    (p instanceof Promise));
        "#
    ));
    assert_eq!(reflected_constructor, ["reflect-constructor:77:false"]);

    let promise_try = run_ok(&format!(
        r#"
        {CHURN}
        Promise.try(function () {{
          churn("try");
          return {{
            get then() {{
              churn("try-then");
              return function (resolve) {{ resolve(23); }};
            }}
          }};
        }}).then(function (v) {{ console.log("try:" + v); }});
        "#
    ));
    assert_eq!(promise_try, ["try:23"]);

    let custom_capabilities = run_ok(&format!(
        r#"
        {CHURN}
        function Capability(executor) {{
          var state = {{ status: "pending", value: undefined }};
          var promise = {{ state: state }};
          executor(
            function (value) {{ churn("custom-resolve"); state.status = "fulfilled"; state.value = value; }},
            function (reason) {{ churn("custom-reject"); state.status = "rejected"; state.value = reason; }}
          );
          return promise;
        }}
        var resolved = Promise.resolve.call(Capability, 41);
        var rejected = Promise.reject.call(Capability, "nope");
        var tried = Promise.try.call(Capability, function () {{ churn("custom-try"); return 43; }});
        console.log("custom:" + resolved.state.status + ":" + resolved.state.value + ":" +
                    rejected.state.status + ":" + rejected.state.value + ":" +
                    tried.state.status + ":" + tried.state.value);
        "#
    ));
    assert_eq!(
        custom_capabilities,
        ["custom:fulfilled:41:rejected:nope:fulfilled:43"]
    );

    let async_return = run_ok(&format!(
        r#"
        {CHURN}
        async function f() {{
          return {{
            get then() {{
              churn("async-return");
              return function (resolve) {{ resolve(17); }};
            }}
          }};
        }}
        f().then(function (v) {{ console.log("async:" + v); }});
        "#
    ));
    assert_eq!(async_return, ["async:17"]);

    let async_generator_yield = run_ok(&format!(
        r#"
        {CHURN}
        var gets = 0;
        Object.defineProperty(Object.prototype, "then", {{
          configurable: true,
          get: function () {{ gets++; churn("async-generator-yield"); return undefined; }}
        }});
        async function* values() {{ yield 5; }}
        var request = values().next();
        delete Object.prototype.then;
        request.then(function (r) {{
          console.log("async-generator:" + r.value + ":" + r.done + ":" + gets);
        }});
        "#
    ));
    assert_eq!(async_generator_yield, ["async-generator:5:false:0"]);

    let async_generator_return = run_ok(&format!(
        r#"
        {CHURN}
        async function* values() {{ yield 1; }}
        var inner = Promise.resolve(9);
        inner.constructor = function OtherPromise() {{}};
        var originalThen = Promise.prototype.then;
        Object.defineProperty(inner, "then", {{
          configurable: true,
          get: function () {{ churn("async-generator-return"); return originalThen; }}
        }});
        values().return(inner).then(function (r) {{
          console.log("async-return:" + r.value + ":" + r.done);
        }});
        "#
    ));
    assert_eq!(async_generator_return, ["async-return:9:true"]);

    let async_from_sync = run_ok(&format!(
        r#"
        {CHURN}
        var calls = 0;
        var iterable = {{
          [Symbol.iterator]: function () {{
            return {{
              next: function () {{
                calls++;
                return Object.defineProperties({{}}, {{
                  done: {{ get: function () {{ churn("afs-done"); return false; }} }},
                  value: {{ get: function () {{
                    return {{
                      get then() {{
                        churn("afs-value");
                        return function (resolve) {{ resolve(31); }};
                      }}
                    }};
                  }} }}
                }});
              }},
              return: function () {{ return {{ value: undefined, done: true }}; }}
            }};
          }}
        }};
        async function first() {{
          for await (var value of iterable) return value;
        }}
        first().then(function (v) {{ console.log("afs:" + v + ":" + calls); }});
        "#
    ));
    assert_eq!(async_from_sync, ["afs:31:1"]);

    let async_dispose = run_ok(&format!(
        r#"
        {CHURN}
        var stack = new AsyncDisposableStack();
        stack.defer(function () {{
          return {{
            get then() {{
              churn("async-dispose");
              return function (resolve) {{ resolve("disposed"); }};
            }}
          }};
        }});
        stack.disposeAsync().then(function () {{ console.log("dispose:done"); }});
        "#
    ));
    assert_eq!(async_dispose, ["dispose:done"]);

    let fixture_dir = std::env::temp_dir().join(format!(
        "zipp-promise-resolution-roots-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&fixture_dir).expect("create dynamic-import fixture directory");
    std::fs::write(
        fixture_dir.join("value.mjs"),
        "export const answer = 47; export function callable() { return answer; }\n",
    )
    .expect("write dynamic-import fixture");
    let imported = zipp_vm::run_with_base(
        r#"
        import("./value.mjs").then(function (ns) {
          console.log("import:" + ns.answer + ":" + ns.callable());
        });
        "#,
        Some(fixture_dir.clone()),
    )
    .expect("dynamic-import source compiles");
    let _ = std::fs::remove_dir_all(&fixture_dir);
    assert!(
        imported.error.is_none(),
        "dynamic import failed: {:?}",
        imported.error
    );
    assert_eq!(imported.output, ["import:47:47"]);
}

#[test]
fn promise_resolution_roots_modes_match() {
    if std::env::var_os("ZIPP_PROMISE_RESOLUTION_ROOTS_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, envs) in [
        ("default", &[][..]),
        ("gc", &[("ZIPP_GC_STRESS", "1")][..]),
        (
            "nojit-gc",
            &[("ZIPP_NOJIT", "1"), ("ZIPP_GC_STRESS", "1")][..],
        ),
        (
            "jit-gc",
            &[("ZIPP_JIT_THRESHOLD", "1"), ("ZIPP_GC_STRESS", "1")][..],
        ),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "promise_resolution_roots_child", "--nocapture"])
            .env("ZIPP_PROMISE_RESOLUTION_ROOTS_CHILD", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env_remove("ZIPP_GC_STRESS");
        for &(key, value) in envs {
            cmd.env(key, value);
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
