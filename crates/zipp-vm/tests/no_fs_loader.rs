//! Observable no-loader semantics shared by the ordinary embedding default and
//! the artifact-only `wasm-no-fs-loader` specialization.

fn run_output(source: &str) -> Vec<String> {
    let outcome = zipp_vm::run(source).expect("source compiles");
    assert_eq!(outcome.error, None, "unexpected top-level throw");
    outcome.output
}

#[test]
fn dynamic_import_preserves_evaluation_coercion_and_rejection_order() {
    let output = run_output(
        r#"
        let marker = {};
        let promise;
        try {
          promise = import(
            (console.log("spec-expr"), {
              toString() { console.log("spec-coerce"); throw marker; }
            }),
            (console.log("opts-expr"), {
              get with() { console.log("with-get"); return {}; }
            })
          );
          console.log("returned", promise instanceof Promise);
        } catch (e) {
          console.log("sync-catch");
        }
        console.log("sync-tail");
        promise.then(
          () => console.log("fulfilled"),
          e => console.log("reject-same", e === marker)
        );
        "#,
    );

    assert_eq!(
        output,
        [
            "spec-expr",
            "opts-expr",
            "spec-coerce",
            "returned true",
            "sync-tail",
            "reject-same true",
        ]
    );
}

#[test]
fn dynamic_import_preserves_options_and_phase_error_precedence() {
    let output = run_output(
        r#"
        function record(label, promise) {
          promise.then(
            () => console.log(label, "fulfilled"),
            e => console.log(
              label,
              e.name,
              e instanceof TypeError,
              e instanceof SyntaxError
            )
          );
        }
        record("normal", import("./missing.mjs"));
        record("bad-options", import("./missing.mjs", 1));
        record("source", import.source("./missing.mjs", {}));
        record("source-bad-options", import.source("./missing.mjs", 1));
        record("defer", import.defer("./missing.mjs", {}));
        console.log("sync-tail");
        "#,
    );

    assert_eq!(
        output,
        [
            "sync-tail",
            "normal TypeError true false",
            "bad-options TypeError true false",
            "source SyntaxError false true",
            "source-bad-options TypeError true false",
            "defer TypeError true false",
        ]
    );
}

#[test]
fn dynamic_import_still_observes_import_option_traps_without_a_loader() {
    let output = run_output(
        r#"
        let attrs = new Proxy({ type: "json" }, {
          ownKeys(target) {
            console.log("ownKeys");
            return Reflect.ownKeys(target);
          },
          getOwnPropertyDescriptor(target, key) {
            console.log("gopd", key);
            return Reflect.getOwnPropertyDescriptor(target, key);
          },
          get(target, key) {
            console.log("get", key);
            return Reflect.get(target, key);
          }
        });
        let options = {
          get with() { console.log("with"); return attrs; },
          get assert() { console.log("assert"); return undefined; }
        };
        import("./missing.mjs", options).then(
          () => console.log("fulfilled"),
          e => console.log("rejected", e.name)
        );
        console.log("sync-tail");
        "#,
    );

    assert_eq!(
        output,
        [
            "with",
            "ownKeys",
            "gopd type",
            "get type",
            "assert",
            "sync-tail",
            "rejected TypeError",
        ]
    );
}

#[test]
fn shadowrealm_import_value_keeps_its_sync_argument_errors_and_async_no_loader_error() {
    let output = run_output(
        r#"
        let realm = new ShadowRealm();
        let marker = {};
        try {
          realm.importValue({ toString() { throw marker; } }, "x");
          console.log("coerce-returned");
        } catch (e) {
          console.log("coerce-sync-same", e === marker);
        }
        try {
          realm.importValue("./missing.mjs", 1);
          console.log("name-returned");
        } catch (e) {
          console.log("name-sync", e.name);
        }
        let promise = realm.importValue("./missing.mjs", "x");
        console.log("returned", promise instanceof Promise);
        promise.then(
          () => console.log("fulfilled"),
          e => console.log("rejected", e.name, e.message)
        );
        console.log("sync-tail");
        "#,
    );

    assert_eq!(
        output,
        [
            "coerce-sync-same true",
            "name-sync TypeError",
            "returned true",
            "sync-tail",
            "rejected TypeError ShadowRealm.prototype.importValue: no module base",
        ]
    );
}

#[cfg(feature = "wasm-no-fs-loader")]
#[test]
fn artifact_public_filesystem_loader_boundaries_fail_closed() {
    let mut state = zipp_vm::embed::compile_script("0").expect("source compiles");
    let error = state
        .set_confined_module_loader(std::path::Path::new("."), std::path::Path::new("."), 1024)
        .expect_err("artifact loader request must fail");
    assert_eq!(error, "filesystem module loader is disabled in this build");

    let error = match zipp_vm::run_module_file(
        std::path::Path::new("this-path-must-not-be-read.mjs"),
        None,
    ) {
        Err(error) => error,
        Ok(_) => panic!("artifact module-file entry must fail"),
    };
    assert_eq!(error, "filesystem module loader is disabled in this build");

    let error = match zipp_vm::run_module_with_base(
        r#"
        import "this-static-dependency-must-not-be-read.mjs";
        console.log("module body must not run");
        "#,
        Some(std::path::PathBuf::from("this-base-must-not-be-read")),
    ) {
        Err(error) => error,
        Ok(_) => panic!("a direct module with a static request must fail before its body"),
    };
    assert_eq!(error, "filesystem module loader is disabled in this build");
}
