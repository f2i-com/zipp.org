//! The hidden release-PGO host policy executes one already-compiled main source
//! with normal JITs, but refuses every runtime source compiler and module loader.

fn run_training(source: &str) -> zipp_vm::Outcome {
    zipp_vm::run_for_pgo_training(source, None).expect("main training source compiles")
}

fn assert_caught_denial(expression: &str) {
    let source = format!(
        r#"
        try {{
          {expression};
          console.log("LEAK");
        }} catch (error) {{
          console.log("DENIED:" + error.name);
        }}
        "#
    );
    let outcome = run_training(&source);
    assert!(
        outcome.error.is_none(),
        "unexpected uncaught error: {:?}",
        outcome.error
    );
    assert_eq!(
        outcome.output.len(),
        1,
        "unexpected output: {:?}",
        outcome.output
    );
    assert!(
        outcome.output[0].starts_with("DENIED:"),
        "{:?}",
        outcome.output
    );
    assert!(!outcome.output.iter().any(|line| line.contains("LEAK")));
}

#[test]
fn pgo_policy_rejects_eval_function_family_and_source_hooks() {
    for expression in [
        r#"globalThis["e" + "val"]("console.log('LEAK')")"#,
        r#"globalThis["F" + "unction"]("console.log('LEAK')")()"#,
        r#"(() => {}).constructor("console.log('LEAK')")()"#,
        r#"Object.getPrototypeOf(async function () {}).constructor("console.log('LEAK')")()"#,
        r#"Object.getPrototypeOf(function* () {}).constructor("console.log('LEAK')")()"#,
        r#"Object.getPrototypeOf(async function* () {}).constructor("console.log('LEAK')")()"#,
        r#"new ShadowRealm().evaluate("console.log('LEAK')")"#,
        r#"$262.evalScript("console.log('LEAK')")"#,
        r#"$262.agent.start("console.log('LEAK')")"#,
    ] {
        assert_caught_denial(expression);
    }
}

#[test]
fn function_policy_rejects_before_parameter_coercion_or_parsing() {
    let outcome = run_training(
        r#"
        var coerced = 0;
        var malformedParameters = {
          toString: function () {
            coerced = 1;
            console.log("COERCED");
            return "(".repeat(100000);
          }
        };
        try {
          var Build = globalThis["F" + "unction"];
          Build(malformedParameters, "console.log('LEAK')");
          console.log("LEAK");
        } catch (error) {
          console.log("DENIED:" + coerced + ":" + error.name);
        }
        "#,
    );
    assert!(
        outcome.error.is_none(),
        "unexpected uncaught error: {:?}",
        outcome.error
    );
    assert_eq!(outcome.output, ["DENIED:0:EvalError"]);
}

#[test]
fn pgo_policy_refuses_dynamic_and_shadowrealm_module_reads() {
    let unique = format!(
        "zipp-pgo-boundary-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos()
    );
    let fixture = std::env::temp_dir().join(unique);
    std::fs::create_dir(&fixture).expect("create unique fixture directory");
    let module = fixture.join("scored.mjs");
    std::fs::write(
        &module,
        "console.log('LEAK'); export function marker() { return 'LEAK'; }\n",
    )
    .expect("write module fixture");

    let dynamic = zipp_vm::run_for_pgo_training(
        r#"
        import("./scored.mjs").then(
          function () { console.log("LEAK"); },
          function (error) { console.log("DENIED:" + error.name); }
        );
        "#,
        Some(fixture.clone()),
    )
    .expect("dynamic-import main source compiles");
    assert!(
        dynamic.error.is_none(),
        "dynamic import error: {:?}",
        dynamic.error
    );
    assert_eq!(
        dynamic.output.len(),
        1,
        "unexpected output: {:?}",
        dynamic.output
    );
    assert!(dynamic.output[0].starts_with("DENIED:"));

    let shadow = zipp_vm::run_for_pgo_training(
        r#"
        new ShadowRealm().importValue("./scored.mjs", "marker").then(
          function () { console.log("LEAK"); },
          function (error) { console.log("DENIED:" + error.name); }
        );
        "#,
        Some(fixture.clone()),
    )
    .expect("ShadowRealm importValue main source compiles");
    assert!(
        shadow.error.is_none(),
        "ShadowRealm import error: {:?}",
        shadow.error
    );
    assert_eq!(
        shadow.output.len(),
        1,
        "unexpected output: {:?}",
        shadow.output
    );
    assert!(shadow.output[0].starts_with("DENIED:"));
    assert!(!dynamic
        .output
        .iter()
        .chain(&shadow.output)
        .any(|line| line.contains("LEAK")));

    std::fs::remove_file(module).expect("remove module fixture");
    std::fs::remove_dir(fixture).expect("remove fixture directory");
}

#[test]
fn ordinary_main_training_code_and_jit_policy_are_unchanged() {
    let outcome = run_training(
        r#"
        function sum(limit) {
          var total = 0;
          for (var index = 0; index < limit; index++) total += index;
          return total;
        }
        console.log(sum(10000));
        "#,
    );
    assert!(
        outcome.error.is_none(),
        "unexpected runtime error: {:?}",
        outcome.error
    );
    assert_eq!(outcome.output, ["49995000"]);
}
